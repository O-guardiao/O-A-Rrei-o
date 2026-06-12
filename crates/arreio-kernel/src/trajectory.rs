//! Trajectory Store — Registro de execuções de tarefas no Blackboard.
//!
//! Inspirado no "trajectory window" do Continual Harness (Karten et al., 2026).
//! Cada execução de tarefa (nó DAG) é registrada como tupla `trajectory::{task_id}`
//! para permitir que o Refiner detecte padrões de falha em contratos.
//!
//! O TrajectoryStore faz pruning automático quando o número de entradas excede
//! `max_entries`, removendo as mais antigas por timestamp.

use crate::blackboard::Blackboard;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Tipo de violação de contrato detectada (PVC-Q1.1 DAC Runtime).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationType {
    SchemaMismatch,
    SlaExceeded,
    MissingAuditField,
    UnexpectedFailure,
}

/// Registro de uma violação de contrato.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractViolation {
    pub contract_id: String,
    pub violation_type: ViolationType,
    pub node_id: String,
    pub details: String,
    pub timestamp_ms: u64,
}

/// Status HITL de uma trajetória (PVC-Q1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum HitlStatus {
    #[default]
    NotApplicable,
    Pending,
    Approved,
    Rejected,
    Escalated,
}

/// Decisão humana auditável (PVC-Q1.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanDecision {
    pub task_id: String,
    pub decision: ApprovalDecision,
    pub approver_identity: String,
    pub approver_roles: Vec<String>,
    /// Hash SHA-256 do contexto (estado FSM + DAG + contratos) no momento da decisão.
    pub context_hash: String,
    pub timestamp: u64,
    pub justification: Option<String>,
    pub policy_name: Option<String>,
    pub escalation_level: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ApprovalDecision {
    Approved,
    Rejected,
    Escalated,
}

/// Registro de uma execução de tarefa no Blackboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryEntry {
    /// ID da tarefa DAG (ex: "dag::task_003")
    pub task_id: String,
    /// Timestamp UNIX da execução
    pub timestamp: u64,
    /// Especificação original da tarefa (NL)
    pub specification: String,
    /// Contrato usado (JSON do arreio-contract)
    pub contract: Option<serde_json::Value>,
    /// Código gerado (snippet — pode ser truncado para economizar espaço)
    pub generated_code_snippet: Option<String>,
    /// Hash SHA-256 do código gerado completo (para referência)
    pub code_hash: Option<String>,
    /// Comando de validação executado
    pub validation_cmd: Option<String>,
    /// Resultado da validação
    pub result: TrajectoryResult,
    /// Modelo(s) LLM usado(s) na geração
    pub models_used: Vec<String>,
    /// Tokens consumidos
    pub tokens_consumed: u64,
    /// Duração em milissegundos
    pub duration_ms: u64,
    /// Contador de tentativas (para recovery blocks)
    pub attempt_number: u32,
    /// Violations de contrato detectadas para esta execução (PVC-Q1.1).
    #[serde(default)]
    pub contract_violations: Vec<ContractViolation>,
    /// Status HITL desta execução (PVC-Q1.2).
    #[serde(default)]
    pub hitl_status: HitlStatus,
    /// Decisão humana registrada, se houver (PVC-Q1.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_decision: Option<HumanDecision>,
}

impl Default for TrajectoryEntry {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            timestamp: 0,
            specification: String::new(),
            contract: None,
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Blocked {
                reason: "default".into(),
            },
            models_used: vec![],
            tokens_consumed: 0,
            duration_ms: 0,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: HitlStatus::NotApplicable,
            human_decision: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TrajectoryResult {
    /// Sucesso — build + testes passaram
    Success { test_count: u32, test_passed: u32 },
    /// Falha — build ou testes falharam
    Failure {
        exit_code: i32,
        error_summary: String,
    },
    /// Timeout — hypervisor matou o processo
    Timeout { duration_ms: u64 },
    /// Bloqueado — interceptor bloqueou comando
    Blocked { reason: String },
}

/// Persistência de trajetórias no Blackboard.
/// Usa categoria "trajectory" com chave = task_id + attempt_number (para evitar colisões).
pub struct TrajectoryStore {
    blackboard: Blackboard,
    /// Número máximo de trajetórias mantidas (evita crescimento infinito).
    max_entries: usize,
}

impl TrajectoryStore {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            max_entries: 1000,
        }
    }

    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max;
        self
    }

    /// Gera chave composta: task_id + attempt_number (evita colisão em retries).
    fn composite_key(task_id: &str, attempt: u32) -> String {
        format!("{}__attempt_{}", task_id, attempt)
    }

    /// Registra uma trajetória no Blackboard.
    pub fn record(&self, entry: &TrajectoryEntry) -> Result<()> {
        let key = Self::composite_key(&entry.task_id, entry.attempt_number);
        let value = serde_json::to_value(entry)?;
        self.blackboard.put_tuple("trajectory", &key, value)?;

        // Podar entradas antigas se exceder o limite
        let all = self.list_all_raw();
        if all.len() > self.max_entries {
            let mut sorted: Vec<_> = all.into_iter().collect();
            sorted.sort_by_key(|(_, e)| e.timestamp);
            let to_remove = sorted.len().saturating_sub(self.max_entries);
            for (key, _) in sorted.iter().take(to_remove) {
                let _ = self.blackboard.delete_tuple("trajectory", key);
            }
        }
        Ok(())
    }

    /// Lista todas as trajetórias (raw keys + entries).
    fn list_all_raw(&self) -> Vec<(String, TrajectoryEntry)> {
        self.blackboard
            .search_tuples("trajectory", "")
            .into_iter()
            .filter_map(|(k, v)| {
                serde_json::from_value::<TrajectoryEntry>(v)
                    .ok()
                    .map(|e| (k, e))
            })
            .collect()
    }

    /// Lista todas as trajetórias, ordenadas por timestamp decrescente.
    pub fn list_all(&self) -> Vec<(String, TrajectoryEntry)> {
        let mut all = self.list_all_raw();
        all.sort_by_key(|(_, e)| e.timestamp);
        all.reverse();
        all
    }

    /// Retorna as últimas N trajetórias (janela deslizante).
    pub fn recent(&self, n: usize) -> Vec<TrajectoryEntry> {
        let mut all = self.list_all_raw();
        all.sort_by_key(|(_, e)| e.timestamp);
        all.into_iter()
            .rev()
            .take(n)
            .map(|(_, e)| e)
            .collect()
    }

    /// Retorna trajetórias de uma tarefa específica (ignorando attempt_number).
    pub fn for_task(&self, task_id: &str) -> Vec<TrajectoryEntry> {
        self.list_all_raw()
            .into_iter()
            .filter(|(k, _)| k.starts_with(task_id))
            .map(|(_, e)| e)
            .collect()
    }

    /// Conta trajetórias únicas (por task_id, ignorando attempt).
    pub fn count_unique_tasks(&self) -> usize {
        let unique: HashSet<String> = self
            .list_all_raw()
            .into_iter()
            .map(|(_, e)| e.task_id)
            .collect();
        unique.len()
    }

    /// Registra uma decisão humana no Blackboard e atualiza a trajetória (PVC-Q1.2).
    pub fn record_human_decision(&self, decision: &HumanDecision) -> Result<()> {
        // 1. Grava na categoria hitl_decision para o FSM poller encontrar
        let key = &decision.task_id;
        let payload = serde_json::to_value(decision)?;
        self.blackboard.put_tuple("hitl_decision", key, payload)?;

        // 2. Atualiza a trajetória correspondente
        let entries = self.list_all_raw();
        if let Some((traj_key, mut entry)) = entries
            .into_iter()
            .find(|(_, e)| e.task_id == decision.task_id)
        {
            entry.hitl_status = match decision.decision {
                ApprovalDecision::Approved => HitlStatus::Approved,
                ApprovalDecision::Rejected => HitlStatus::Rejected,
                ApprovalDecision::Escalated => HitlStatus::Escalated,
            };
            entry.human_decision = Some(decision.clone());
            let value = serde_json::to_value(&entry)?;
            self.blackboard.put_tuple("trajectory", &traj_key, value)?;
        }

        Ok(())
    }

    /// Computa hash SHA-256 de contexto para auditoria (PVC-Q1.2).
    /// O contexto é determinístico: serializa estado FSM + DAG snapshot + contratos.
    pub fn compute_context_hash(
        fsm_state: &str,
        dag_nodes: &[String],
        contracts: &[serde_json::Value],
    ) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(fsm_state.as_bytes());
        for node in dag_nodes {
            hasher.update(node.as_bytes());
        }
        for contract in contracts {
            hasher.update(serde_json::to_string(contract)?.as_bytes());
        }
        Ok(format!("{:x}", hasher.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_entry(task_id: &str, timestamp: u64, success: bool) -> TrajectoryEntry {
        TrajectoryEntry {
            task_id: task_id.into(),
            timestamp,
            specification: "Implementar login JWT".into(),
            contract: Some(serde_json::json!({"pre": "user exists", "post": "token is valid"})),
            generated_code_snippet: Some("fn login() {}".into()),
            code_hash: Some("abc123".into()),
            validation_cmd: Some("cargo test".into()),
            result: if success {
                TrajectoryResult::Success {
                    test_count: 5,
                    test_passed: 5,
                }
            } else {
                TrajectoryResult::Failure {
                    exit_code: 1,
                    error_summary: "test failed".into(),
                }
            },
            models_used: vec!["deepseek-v4-pro".into()],
            tokens_consumed: 1500,
            duration_ms: 3200,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: HitlStatus::NotApplicable,
            human_decision: None,
        }
    }

    #[test]
    fn records_and_retrieves() {
        let bb = temp_bb();
        let store = TrajectoryStore::new(bb);
        let entry = make_entry("task_001", 1717000000, true);
        store.record(&entry).unwrap();
        let recent = store.recent(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].task_id, "task_001");
    }

    #[test]
    fn prunes_old_entries() {
        let bb = temp_bb();
        let store = TrajectoryStore::new(bb).with_max_entries(3);
        for i in 0..5 {
            let entry = make_entry(&format!("task_{:03}", i), 1717000000 + i as u64, true);
            store.record(&entry).unwrap();
        }
        let all = store.list_all();
        assert!(all.len() <= 3, "Deve ter no máximo 3 entradas após pruning, tem {}", all.len());
    }

    #[test]
    fn serialization_roundtrip() {
        let result = TrajectoryResult::Success {
            test_count: 10,
            test_passed: 10,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: TrajectoryResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, result);
    }

    #[test]
    fn for_task_filters_correctly() {
        let bb = temp_bb();
        let store = TrajectoryStore::new(bb);
        let e1 = make_entry("task_A", 1717000000, true);
        let e2 = make_entry("task_B", 1717000001, false);
        store.record(&e1).unwrap();
        store.record(&e2).unwrap();
        let a_entries = store.for_task("task_A");
        assert_eq!(a_entries.len(), 1);
        assert_eq!(a_entries[0].task_id, "task_A");
    }

    #[test]
    fn human_decision_serializes_correctly() {
        let decision = HumanDecision {
            task_id: "task_001".into(),
            decision: ApprovalDecision::Approved,
            approver_identity: "admin".into(),
            approver_roles: vec!["admin".into()],
            context_hash: "abc123".into(),
            timestamp: 1717000000,
            justification: Some("looks good".into()),
            policy_name: Some("financial_tx".into()),
            escalation_level: 0,
        };
        let json = serde_json::to_string(&decision).unwrap();
        let parsed: HumanDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_id, "task_001");
        assert_eq!(parsed.decision, ApprovalDecision::Approved);
    }

    #[test]
    fn trajectory_store_records_human_decision() {
        let bb = temp_bb();
        let store = TrajectoryStore::new(bb.clone());
        let entry = make_entry("task_002", 1717000000, true);
        store.record(&entry).unwrap();

        let decision = HumanDecision {
            task_id: "task_002".into(),
            decision: ApprovalDecision::Rejected,
            approver_identity: "auditor".into(),
            approver_roles: vec!["auditor".into()],
            context_hash: "def456".into(),
            timestamp: 1717000001,
            justification: Some("too risky".into()),
            policy_name: None,
            escalation_level: 1,
        };
        store.record_human_decision(&decision).unwrap();

        // Verifica que hitl_decision foi gravado no Blackboard
        let raw = bb.get_tuple("hitl_decision", "task_002").unwrap();
        let parsed: HumanDecision = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.decision, ApprovalDecision::Rejected);

        // Verifica que TrajectoryEntry foi atualizado
        let updated = store.for_task("task_002");
        assert_eq!(updated[0].hitl_status, HitlStatus::Rejected);
        assert!(updated[0].human_decision.is_some());
    }

    #[test]
    fn context_hash_is_deterministic() {
        let h1 = TrajectoryStore::compute_context_hash(
            "Execution",
            &["node1".into(), "node2".into()],
            &[serde_json::json!({"id": "c1"})],
        )
        .unwrap();
        let h2 = TrajectoryStore::compute_context_hash(
            "Execution",
            &["node1".into(), "node2".into()],
            &[serde_json::json!({"id": "c1"})],
        )
        .unwrap();
        assert_eq!(h1, h2, "Hash deve ser determinístico para mesmo contexto");
    }

    #[test]
    fn context_hash_changes_with_different_state() {
        let h1 = TrajectoryStore::compute_context_hash(
            "Execution",
            &["node1".into()],
            &[],
        )
        .unwrap();
        let h2 = TrajectoryStore::compute_context_hash(
            "ComplianceCheck",
            &["node1".into()],
            &[],
        )
        .unwrap();
        assert_ne!(h1, h2, "Hash deve mudar quando estado FSM muda");
    }
}
