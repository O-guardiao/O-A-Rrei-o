//! HITL — Human-in-the-Loop Compliance Gate (PVC-Q1.2).
//!
//! O FSM pausa em `AwaitingHumanInput` e polling do Blackboard até que um
//! operador humano grave sua decisão. Todo o mecanismo é síncrono — não usa
//! async/await nem tokio channels.

use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::{thread, time::Duration};

// ── Compliance Checker ────────────────────────────────────────────────────────

/// Resultado da verificação de compliance para uma ação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplianceResult {
    /// Ação é segura — prossegue sem HITL.
    AutoApprove,
    /// Ação requer aprovação humana.
    RequireApproval {
        approvers: Vec<String>,
        timeout_sec: u64,
        policy_name: Option<String>,
    },
    /// Ação é proibida — aborta imediatamente.
    AutoReject { reason: String },
}

/// Verifica se uma ação DAG precisa de aprovação humana.
///
/// Integra com:
/// - Escalation policies do `arreio-security` (recebidas como parâmetro)
/// - Contracts do PVC-Q1.1 (`contract.requires_approval`)
/// - Tool category (Destructive, Network, etc.)
#[derive(Debug, Clone, Default)]
pub struct ComplianceChecker;

impl ComplianceChecker {
    pub fn new() -> Self {
        Self
    }

    /// Verifica compliance de um nó DAG dado as policies ativas e contracts.
    ///
    /// # Arguments
    /// * `tool_name` — nome da tool a ser executada
    /// * `contracts` — lista de contracts aplicáveis (do Q1.1)
    /// * `policies` — policies de escalation ativas (do `EscalationEngine`)
    /// * `cost_estimate_usd` — estimativa de custo, se disponível
    pub fn check(
        &self,
        tool_name: &str,
        contracts: &[RequiresApproval],
        policies: &[EscalationPolicyRef],
        cost_estimate_usd: Option<f64>,
    ) -> ComplianceResult {
        // 1. Contracts do Q1.1 que exigem aprovação
        for contract in contracts {
            if contract.requires_approval {
                return ComplianceResult::RequireApproval {
                    approvers: contract.approvers.clone(),
                    timeout_sec: contract.timeout_sec,
                    policy_name: Some(format!("contract:{}", contract.contract_id)),
                };
            }
        }

        // 2. Escalation policies (ordem: mais específica primeiro)
        for policy in policies {
            if policy.matches(tool_name, cost_estimate_usd) {
                return match policy.action {
                    PolicyActionRef::RequireApproval => ComplianceResult::RequireApproval {
                        approvers: policy.approvers.clone(),
                        timeout_sec: policy.timeout_sec,
                        policy_name: Some(policy.name.clone()),
                    },
                    PolicyActionRef::AutoReject => ComplianceResult::AutoReject {
                        reason: format!("policy '{}' auto-rejects tool '{}'", policy.name, tool_name),
                    },
                    PolicyActionRef::LogOnly => continue,
                };
            }
        }

        // 3. Padrão: auto-aprovação
        ComplianceResult::AutoApprove
    }
}

// ── Human Input Poller ────────────────────────────────────────────────────────

/// Resultado do polling de decisão humana.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollResult {
    /// Ainda aguardando decisão.
    Pending,
    /// Aprovação recebida.
    Approved {
        approver: String,
        timestamp: u64,
        justification: Option<String>,
    },
    /// Rejeição recebida.
    Rejected {
        approver: String,
        timestamp: u64,
        reason: Option<String>,
    },
    /// Timeout atingido sem decisão.
    Timeout,
}

/// Polling síncrono do Blackboard por decisão humana.
///
/// Não usa async/await. Loop com sleep de 100ms entre verificações.
pub struct HumanInputPoller;

impl HumanInputPoller {
    /// Polling interval entre verificações do Blackboard.
    pub const POLL_INTERVAL_MS: u64 = 100;

    /// Aguarda decisão humana no Blackboard até timeout.
    ///
    /// A decisão é esperada na categoria `"hitl_decision"` com chave `task_id`.
    /// O payload deve ser um JSON com campo `"decision"` ("Approved" | "Rejected").
    pub fn poll(
        blackboard: &Blackboard,
        task_id: &str,
        timeout_ms: u64,
    ) -> Result<PollResult> {
        let start = std::time::Instant::now();
        let interval = Duration::from_millis(Self::POLL_INTERVAL_MS);

        loop {
            // Verifica se há decisão no Blackboard
            if let Some(value) = blackboard.get_tuple("hitl_decision", task_id) {
                if let Ok(decision) = serde_json::from_value::<HitlDecisionPayload>(value) {
                    return Ok(match decision.decision.as_str() {
                        "Approved" => PollResult::Approved {
                            approver: decision.approver,
                            timestamp: decision.timestamp,
                            justification: decision.justification,
                        },
                        "Rejected" => PollResult::Rejected {
                            approver: decision.approver,
                            timestamp: decision.timestamp,
                            reason: decision.justification,
                        },
                        _ => PollResult::Pending,
                    });
                }
            }

            // Verifica timeout
            let elapsed = start.elapsed().as_millis() as u64;
            if elapsed >= timeout_ms {
                return Ok(PollResult::Timeout);
            }

            // Sleep antes da próxima verificação
            thread::sleep(interval);
        }
    }
}

// ── Tipos Auxiliares (referências leves) ──────────────────────────────────────

/// Referência a um contract que pode exigir aprovação.
/// Passado pelo caller (integração com Q1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiresApproval {
    pub contract_id: String,
    pub requires_approval: bool,
    pub approvers: Vec<String>,
    pub timeout_sec: u64,
}

/// Referência a uma escalation policy ativa.
/// Passado pelo caller (`EscalationEngine` do arreio-security).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationPolicyRef {
    pub name: String,
    pub action: PolicyActionRef,
    pub approvers: Vec<String>,
    pub timeout_sec: u64,
}

impl EscalationPolicyRef {
    /// Verifica se esta policy se aplica a uma tool/custo.
    pub fn matches(&self, tool_name: &str, cost_estimate_usd: Option<f64>) -> bool {
        // Tool name matching (simplificado — tool exato ou prefixo)
        if self.name.contains(tool_name) {
            return true;
        }
        // Custo matching (simplificado — apenas verifica se há threshold)
        if let Some(_cost) = cost_estimate_usd {
            // Em implementação real, parseria a condição do YAML ("> 100.00")
            // Por enquanto, qualquer cost > 0 dispara se a policy mencionar cost
            if self.name.contains("cost") || self.name.contains("financial") {
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyActionRef {
    RequireApproval,
    AutoReject,
    LogOnly,
}

/// Payload esperado no Blackboard para decisão HITL.
#[derive(Debug, Clone, Deserialize)]
struct HitlDecisionPayload {
    decision: String,
    approver: String,
    timestamp: u64,
    justification: Option<String>,
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::path::Path;

    #[test]
    fn compliance_checker_auto_approves_safe_tool() {
        let checker = ComplianceChecker::new();
        let result = checker.check("read_file", &[], &[], None);
        assert_eq!(result, ComplianceResult::AutoApprove);
    }

    #[test]
    fn compliance_checker_detects_contract_requiring_approval() {
        let checker = ComplianceChecker::new();
        let contracts = vec![RequiresApproval {
            contract_id: "c1".into(),
            requires_approval: true,
            approvers: vec!["admin".into()],
            timeout_sec: 300,
        }];
        let result = checker.check("any_tool", &contracts, &[], None);
        assert!(matches!(result, ComplianceResult::RequireApproval { .. }));
    }

    #[test]
    fn compliance_checker_respects_escalation_policy() {
        let checker = ComplianceChecker::new();
        let policies = vec![EscalationPolicyRef {
            name: "db_delete_policy".into(),
            action: PolicyActionRef::RequireApproval,
            approvers: vec!["data_owner".into()],
            timeout_sec: 600,
        }];
        let result = checker.check("db_delete", &[], &policies, None);
        assert!(matches!(result, ComplianceResult::RequireApproval { .. }));
    }

    #[test]
    fn compliance_checker_auto_rejects_when_policy_says_so() {
        let checker = ComplianceChecker::new();
        let policies = vec![EscalationPolicyRef {
            name: "rm_rf_policy".into(),
            action: PolicyActionRef::AutoReject,
            approvers: vec![],
            timeout_sec: 0,
        }];
        let result = checker.check("rm_rf", &[], &policies, None);
        assert!(matches!(result, ComplianceResult::AutoReject { .. }));
    }

    #[test]
    fn human_input_poller_returns_approved_when_decision_found() {
        let tmp = tempfile::tempdir().unwrap();
        let bb = Blackboard::open(Path::new("/dev/null")).unwrap_or_else(|_| {
            // Fallback para arquivo temporário se /dev/null falhar (Windows)
            Blackboard::open(&tmp.path().join("bb.json")).unwrap()
        });

        // Simula decisão gravada no Blackboard
        let decision = serde_json::json!({
            "decision": "Approved",
            "approver": "admin",
            "timestamp": 1234567890u64,
            "justification": "looks good"
        });
        bb.put_tuple("hitl_decision", "task_001", decision).unwrap();

        let result = HumanInputPoller::poll(&bb, "task_001", 5000).unwrap();
        assert!(matches!(result, PollResult::Approved { approver, .. } if approver == "admin"));
    }

    #[test]
    fn human_input_poller_returns_timeout_after_deadline() {
        let tmp = tempfile::tempdir().unwrap();
        let bb = Blackboard::open(&tmp.path().join("bb.json")).unwrap();

        // Sem decisão no Blackboard — deve retornar timeout rapidamente
        let result = HumanInputPoller::poll(&bb, "task_002", 50).unwrap();
        assert_eq!(result, PollResult::Timeout);
    }

    #[test]
    fn human_input_poller_returns_rejected_when_rejection_found() {
        let tmp = tempfile::tempdir().unwrap();
        let bb = Blackboard::open(&tmp.path().join("bb.json")).unwrap();

        let decision = serde_json::json!({
            "decision": "Rejected",
            "approver": "auditor",
            "timestamp": 1234567890u64,
            "justification": "too risky"
        });
        bb.put_tuple("hitl_decision", "task_003", decision).unwrap();

        let result = HumanInputPoller::poll(&bb, "task_003", 5000).unwrap();
        assert!(matches!(result, PollResult::Rejected { approver, .. } if approver == "auditor"));
    }
}
