//! Refiner — Quarto ator do Arreio. Detecta contratos com falha repetida
//! e re-deriva contratos a partir da janela de trajetória (Blackboard).
//!
//! Inspirado no mecanismo de "harness evolution" do Continual Harness
//! (Karten et al., 2026). Adaptado para o modelo stateless do Arreio:
//! o Refiner lê tuplas "trajectory" do Blackboard, detecta contratos que
//! falharam 3+ vezes com 2+ modelos diferentes, e publica novos contratos
//! na categoria "contract".
//!
//! Princípio: se o mesmo contrato falha com múltiplos modelos, o problema
//! está no contrato, não no modelo. O Refiner fecha o loop de feedback
//! que faltava no Arreio.

use arreio_dag::Contract;
use arreio_kernel::trajectory::{TrajectoryEntry, TrajectoryResult, TrajectoryStore};
use arreio_kernel::{Blackboard, ContractViolation, ViolationType};
use arreio_provider::ProviderClient;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Constantes de threshold para detecção de falha de contrato.
const CONTRACT_FAILURE_THRESHOLD: u32 = 3;
const MIN_ATTEMPTS: u32 = 3;
const MIN_DISTINCT_MODELS: usize = 2;
const TRAJECTORY_WINDOW_SIZE: usize = 100;

/// Padrão de falha detectado pelo Refiner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContractFailure {
    /// Hash SHA-256 do contrato que falhou
    pub contract_hash: String,
    /// Número de falhas observadas
    pub failure_count: u32,
    /// Número total de tentativas com este contrato
    pub total_attempts: u32,
    /// Modelos distintos que falharam com este contrato
    pub distinct_failing_models: Vec<String>,
    /// IDs das tarefas que falharam
    pub failing_task_ids: Vec<String>,
    /// Recomendação do Refiner
    pub recommendation: RefinerAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RefinerAction {
    /// Contrato deve ser re-derivado da especificação original
    ReDeriveContract,
    /// Especificação original é ambígua — requer intervenção humana
    EscalateToHuman { reason: String },
    /// Padrão de falha é inconsistente — continuar observando
    ContinueObserving,
}

/// Resultado da execução do Refiner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinerReport {
    pub analyzed_trajectories: u32,
    pub contracts_evaluated: u32,
    pub failures_detected: Vec<ContractFailure>,
    pub actions_taken: Vec<RefinerActionTaken>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinerActionTaken {
    pub contract_hash: String,
    pub action: RefinerAction,
    pub new_contract: Option<serde_json::Value>,
}

pub struct Refiner {
    blackboard: Blackboard,
    trajectory_store: TrajectoryStore,
    /// Provider opcional — se None, Refiner só detecta falhas, não re-deriva.
    provider: Option<Box<dyn ProviderClient>>,
}

impl Refiner {
    /// Cria um Refiner com provider (capaz de re-derivar contratos).
    pub fn new(blackboard: Blackboard, provider: Box<dyn ProviderClient>) -> Self {
        let trajectory_store = TrajectoryStore::new(blackboard.clone());
        Self {
            blackboard,
            trajectory_store,
            provider: Some(provider),
        }
    }

    /// Cria um Refiner sem provider (apenas detecção de falhas — útil para testes).
    pub fn detect_only(blackboard: Blackboard) -> Self {
        let trajectory_store = TrajectoryStore::new(blackboard.clone());
        Self {
            blackboard,
            trajectory_store,
            provider: None,
        }
    }

    /// Executa uma rodada de análise da janela de trajetória.
    /// Retorna o relatório e publica no Blackboard em "refiner::last_report".
    pub fn analyze(&self) -> anyhow::Result<RefinerReport> {
        let trajectories = self.trajectory_store.recent(TRAJECTORY_WINDOW_SIZE);
        let failures = self.detect_contract_failures(&trajectories);

        let mut actions = Vec::new();
        for failure in &failures {
            if failure.recommendation == RefinerAction::ReDeriveContract {
                if let Some(ref provider) = self.provider {
                    let action = self.re_derive_contract(failure, provider)?;
                    actions.push(action);
                } else {
                    actions.push(RefinerActionTaken {
                        contract_hash: failure.contract_hash.clone(),
                        action: RefinerAction::ReDeriveContract,
                        new_contract: None,
                    });
                }
            }
        }

        let report = RefinerReport {
            analyzed_trajectories: trajectories.len() as u32,
            contracts_evaluated: count_unique_contracts(&trajectories) as u32,
            failures_detected: failures,
            actions_taken: actions,
            timestamp: now_epoch_secs(),
        };

        // Publica o relatório no Blackboard
        let value = serde_json::to_value(&report)?;
        self.blackboard.put_tuple("refiner", "last_report", value)?;

        Ok(report)
    }

    /// Detecta contratos que falharam repetidamente com modelos diferentes.
    /// Esta função é pública para permitir testes unitários com trajetórias sintéticas.
    pub fn detect_contract_failures(
        &self,
        trajectories: &[TrajectoryEntry],
    ) -> Vec<ContractFailure> {
        // Agrupa trajetórias por contrato (usando hash SHA-256)
        let mut by_contract: HashMap<String, Vec<&TrajectoryEntry>> = HashMap::new();
        for t in trajectories {
            if let Some(ref contract) = t.contract {
                let hash = hash_contract(contract);
                by_contract.entry(hash).or_default().push(t);
            }
        }

        let mut failures = Vec::new();

        for (contract_hash, entries) in &by_contract {
            let failed: Vec<_> = entries
                .iter()
                .filter(|e| matches!(e.result, TrajectoryResult::Failure { .. }))
                .collect();

            // Threshold mínimo de falhas
            if failed.len() < CONTRACT_FAILURE_THRESHOLD as usize {
                continue;
            }

            // Mínimo de tentativas totais
            if entries.len() < MIN_ATTEMPTS as usize {
                continue;
            }

            // Modelos distintos que falharam
            let mut failing_models: Vec<String> = failed
                .iter()
                .flat_map(|e| e.models_used.clone())
                .collect();
            failing_models.sort();
            failing_models.dedup();

            if failing_models.len() < MIN_DISTINCT_MODELS {
                continue; // Pode ser viés do modelo, não do contrato
            }

            let failing_task_ids: Vec<String> =
                failed.iter().map(|e| e.task_id.clone()).collect();

            let recommendation = if entries.len() >= 5
                && failed.len() as f64 / entries.len() as f64 > 0.8
            {
                // 80%+ de falha com 5+ tentativas → especificação ambígua
                RefinerAction::EscalateToHuman {
                    reason: format!(
                        "Contrato falhou em {}/{} tentativas com {} modelos distintos",
                        failed.len(),
                        entries.len(),
                        failing_models.len()
                    ),
                }
            } else {
                RefinerAction::ReDeriveContract
            };

            failures.push(ContractFailure {
                contract_hash: contract_hash.clone(),
                failure_count: failed.len() as u32,
                total_attempts: entries.len() as u32,
                distinct_failing_models: failing_models,
                failing_task_ids,
                recommendation,
            });
        }

        failures
    }

    /// Re-deriva um contrato usando a especificação original e o provider.
    fn re_derive_contract(
        &self,
        failure: &ContractFailure,
        provider: &Box<dyn ProviderClient>,
    ) -> anyhow::Result<RefinerActionTaken> {
        // Busca a especificação original da primeira tarefa que falhou
        let trajectory = self
            .trajectory_store
            .for_task(&failure.failing_task_ids[0])
            .into_iter()
            .next();

        let specification = match trajectory {
            Some(t) => t.specification,
            None => {
                return Ok(RefinerActionTaken {
                    contract_hash: failure.contract_hash.clone(),
                    action: RefinerAction::ContinueObserving,
                    new_contract: None,
                });
            }
        };

        let revised_prompt = format!(
            "The following specification previously failed validation {} times \
             with {} different models. The contract derived from it was likely incorrect. \
             Please re-derive a more precise contract with stricter pre/post conditions.\n\n\
             Original specification:\n{}\n\n\
             Previous failures involved tasks: {:?}\n\n\
             Return ONLY a JSON object with 'preconditions', 'postconditions', and 'invariants' arrays.",
            failure.failure_count,
            failure.distinct_failing_models.len(),
            specification,
            failure.failing_task_ids,
        );

        let req = arreio_provider::ChatRequest {
            model: String::new(),
            system: String::new(),
            user: revised_prompt,
            messages: vec![],
            tools: None,
        };
        let resp = provider.chat(req)?;

        // Tenta parsear como JSON, com fallback
        let new_contract: serde_json::Value = serde_json::from_str(&resp.content).unwrap_or_else(|_| {
            serde_json::json!({
                "raw_response": resp.content,
                "derived_at": now_epoch_secs(),
                "derived_from_spec": specification,
            })
        });

        // Publica o novo contrato no Blackboard
        let contract_hash = hash_contract(&new_contract);
        self.blackboard.put_tuple(
            "contract",
            &contract_hash,
            new_contract.clone(),
        )?;

        // Publica evento de correção como tuple (sem pub/sub de eventos)
        let _ = self.blackboard.put_tuple(
            "refiner_event",
            "contract_re_derived",
            serde_json::json!({
                "old_contract_hash": failure.contract_hash,
                "new_contract_hash": contract_hash,
                "specification": specification,
                "failure_count": failure.failure_count,
            }),
        );

        Ok(RefinerActionTaken {
            contract_hash: failure.contract_hash.clone(),
            action: RefinerAction::ReDeriveContract,
            new_contract: Some(new_contract),
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

fn hash_contract(contract: &serde_json::Value) -> String {
    // Hash simples baseado na representação JSON canônica
    let json = serde_json::to_string(contract).expect("falha ao serializar contrato para hash");
    // Usa o comprimento + primeiros 32 chars como "hash leve" para testes
    // (evita dependência de sha2 para manter as deps enxutas)
    if json.len() <= 32 {
        json
    } else {
        format!("{}..{}", &json[..16], &json[json.len() - 16..])
    }
}

/// Verifica um TrajectoryEntry contra um conjunto de contracts.
/// Retorna todas as violações detectadas.
/// Determinístico: não chama LLM, não faz I/O.
/// Latência esperada: < 1ms por nó.
pub fn check_contracts(
    entry: &TrajectoryEntry,
    contracts: &[Contract],
) -> Vec<ContractViolation> {
    let mut violations = Vec::new();
    let now = now_epoch_secs() * 1000;

    for contract in contracts {
        // 1. Verificar SLA de latência
        if entry.duration_ms > contract.sla_latency_ms {
            violations.push(ContractViolation {
                contract_id: contract.id.clone(),
                violation_type: ViolationType::SlaExceeded,
                node_id: entry.task_id.clone(),
                details: format!(
                    "duration_ms={} > sla_latency_ms={}",
                    entry.duration_ms, contract.sla_latency_ms
                ),
                timestamp_ms: now,
            });
        }

        // 2. Verificar se falha está na taxonomy
        if let TrajectoryResult::Failure { exit_code, error_summary } = &entry.result {
            let known = contract.failure_taxonomy.iter().any(|fm| {
                error_summary.to_lowercase().contains(&fm.name.to_lowercase())
            });
            if !known {
                violations.push(ContractViolation {
                    contract_id: contract.id.clone(),
                    violation_type: ViolationType::UnexpectedFailure,
                    node_id: entry.task_id.clone(),
                    details: format!(
                        "exit_code={}, error_summary='{}' não está na taxonomia",
                        exit_code, error_summary
                    ),
                    timestamp_ms: now,
                });
            }
        }

        // 3. Verificar audit fields no entry.contract
        if let Some(ref entry_contract) = entry.contract {
            for field in &contract.audit_footprint {
                if entry_contract.get(field).is_none() {
                    violations.push(ContractViolation {
                        contract_id: contract.id.clone(),
                        violation_type: ViolationType::MissingAuditField,
                        node_id: entry.task_id.clone(),
                        details: format!(
                            "Campo obrigatório '{}' ausente no contract da trajectory",
                            field
                        ),
                        timestamp_ms: now,
                    });
                }
            }
        }
    }

    violations
}

fn count_unique_contracts(trajectories: &[TrajectoryEntry]) -> usize {
    let hashes: HashSet<String> = trajectories
        .iter()
        .filter_map(|t| t.contract.as_ref().map(hash_contract))
        .collect();
    hashes.len()
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trajectory(
        task_id: &str,
        contract_hash: &str,
        model: &str,
        success: bool,
    ) -> TrajectoryEntry {
        TrajectoryEntry {
            task_id: task_id.into(),
            timestamp: 1717000000,
            specification: "Test spec".into(),
            contract: Some(serde_json::json!({"hash": contract_hash})),
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: if success {
                TrajectoryResult::Success {
                    test_count: 1,
                    test_passed: 1,
                }
            } else {
                TrajectoryResult::Failure {
                    exit_code: 1,
                    error_summary: "test failed".into(),
                }
            },
            models_used: vec![model.into()],
            tokens_consumed: 100,
            duration_ms: 1000,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: arreio_kernel::HitlStatus::NotApplicable,
            human_decision: None,
        }
    }

    fn temp_refiner() -> Refiner {
        let f = tempfile::NamedTempFile::new().unwrap();
        let p: std::path::PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        Refiner::detect_only(bb)
    }

    #[test]
    fn detects_contract_failure_with_multiple_models() {
        let refiner = temp_refiner();
        let trajectories = vec![
            make_trajectory("t1", "contract_A", "deepseek-v4-pro", false),
            make_trajectory("t2", "contract_A", "deepseek-v4-pro", false),
            make_trajectory("t3", "contract_A", "claude-opus-4", false),
            make_trajectory("t4", "contract_B", "deepseek-v4-pro", true),
        ];
        let failures = refiner.detect_contract_failures(&trajectories);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failure_count, 3);
        assert!(failures[0].distinct_failing_models.len() >= 2);
        assert_eq!(failures[0].recommendation, RefinerAction::ReDeriveContract);
    }

    #[test]
    fn ignores_single_model_failure() {
        let refiner = temp_refiner();
        let trajectories = vec![
            make_trajectory("t1", "contract_A", "deepseek-v4-pro", false),
            make_trajectory("t2", "contract_A", "deepseek-v4-pro", false),
            make_trajectory("t3", "contract_A", "deepseek-v4-pro", false),
        ];
        let failures = refiner.detect_contract_failures(&trajectories);
        assert!(
            failures.is_empty(),
            "Single model failure should not trigger"
        );
    }

    #[test]
    fn escalates_when_failure_rate_exceeds_80_percent() {
        let refiner = temp_refiner();
        let mut trajectories = Vec::new();
        for i in 0..6 {
            trajectories.push(make_trajectory(
                &format!("t{}", i),
                "contract_X",
                if i % 2 == 0 {
                    "deepseek-v4-pro"
                } else {
                    "claude-opus-4"
                },
                i == 5, // só o último é sucesso (1/6 = 16.7% sucesso = 83.3% falha > 80%)
            ));
        }
        let failures = refiner.detect_contract_failures(&trajectories);
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            failures[0].recommendation,
            RefinerAction::EscalateToHuman { .. }
        ));
    }

    #[test]
    fn ignores_below_threshold() {
        let refiner = temp_refiner();
        let trajectories = vec![
            make_trajectory("t1", "contract_X", "model-A", false),
            make_trajectory("t2", "contract_X", "model-B", false),
            // Apenas 2 falhas — abaixo do threshold de 3
        ];
        let failures = refiner.detect_contract_failures(&trajectories);
        assert!(failures.is_empty());
    }

    // ── Testes de Contract Enforcer (PVC-Q1.1) ─────────────────────────────


    #[test]
    fn check_contracts_detects_sla_exceeded() {
        let contract = Contract {
            id: "c1".into(),
            schema: serde_json::json!({}),
            sla_latency_ms: 100,
            failure_taxonomy: vec![],
            audit_footprint: vec![],
        };
        let entry = TrajectoryEntry {
            task_id: "t1".into(),
            timestamp: 0,
            specification: "spec".into(),
            contract: None,
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Success { test_count: 1, test_passed: 1 },
            models_used: vec![],
            tokens_consumed: 0,
            duration_ms: 500, // > 100 SLA
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: arreio_kernel::HitlStatus::NotApplicable,
            human_decision: None,
        };
        let violations = check_contracts(&entry, &[contract]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].violation_type, ViolationType::SlaExceeded);
    }

    #[test]
    fn check_contracts_detects_unexpected_failure() {
        let contract = Contract {
            id: "c1".into(),
            schema: serde_json::json!({}),
            sla_latency_ms: 1000,
            failure_taxonomy: vec![
                arreio_dag::FailureMode {
                    name: "timeout".into(),
                    severity: arreio_dag::FailureSeverity::Medium,
                    description: "".into(),
                },
            ],
            audit_footprint: vec![],
        };
        let entry = TrajectoryEntry {
            task_id: "t1".into(),
            timestamp: 0,
            specification: "spec".into(),
            contract: None,
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Failure {
                exit_code: 1,
                error_summary: "segmentation fault".into(), // não está na taxonomy
            },
            models_used: vec![],
            tokens_consumed: 0,
            duration_ms: 100,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: arreio_kernel::HitlStatus::NotApplicable,
            human_decision: None,
        };
        let violations = check_contracts(&entry, &[contract]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].violation_type, ViolationType::UnexpectedFailure);
    }

    #[test]
    fn check_contracts_ignores_known_failure() {
        let contract = Contract {
            id: "c1".into(),
            schema: serde_json::json!({}),
            sla_latency_ms: 1000,
            failure_taxonomy: vec![
                arreio_dag::FailureMode {
                    name: "timeout".into(),
                    severity: arreio_dag::FailureSeverity::Medium,
                    description: "".into(),
                },
            ],
            audit_footprint: vec![],
        };
        let entry = TrajectoryEntry {
            task_id: "t1".into(),
            timestamp: 0,
            specification: "spec".into(),
            contract: None,
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Failure {
                exit_code: 1,
                error_summary: "connection timeout".into(), // está na taxonomy
            },
            models_used: vec![],
            tokens_consumed: 0,
            duration_ms: 100,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: arreio_kernel::HitlStatus::NotApplicable,
            human_decision: None,
        };
        let violations = check_contracts(&entry, &[contract]);
        assert!(violations.is_empty(), "Falha conhecida não deve gerar violação");
    }

    #[test]
    fn check_contracts_detects_missing_audit_field() {
        let contract = Contract {
            id: "c1".into(),
            schema: serde_json::json!({}),
            sla_latency_ms: 1000,
            failure_taxonomy: vec![],
            audit_footprint: vec!["author".into(), "reviewer".into()],
        };
        let entry = TrajectoryEntry {
            task_id: "t1".into(),
            timestamp: 0,
            specification: "spec".into(),
            contract: Some(serde_json::json!({"author": "Alice"})), // falta "reviewer"
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Success { test_count: 1, test_passed: 1 },
            models_used: vec![],
            tokens_consumed: 0,
            duration_ms: 100,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: arreio_kernel::HitlStatus::NotApplicable,
            human_decision: None,
        };
        let violations = check_contracts(&entry, &[contract]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].violation_type, ViolationType::MissingAuditField);
        assert!(violations[0].details.contains("reviewer"));
    }

    #[test]
    fn check_contracts_no_violations_when_compliant() {
        let contract = Contract {
            id: "c1".into(),
            schema: serde_json::json!({}),
            sla_latency_ms: 1000,
            failure_taxonomy: vec![],
            audit_footprint: vec![],
        };
        let entry = TrajectoryEntry {
            task_id: "t1".into(),
            timestamp: 0,
            specification: "spec".into(),
            contract: None,
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Success { test_count: 1, test_passed: 1 },
            models_used: vec![],
            tokens_consumed: 0,
            duration_ms: 100,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: arreio_kernel::HitlStatus::NotApplicable,
            human_decision: None,
        };
        let violations = check_contracts(&entry, &[contract]);
        assert!(violations.is_empty());
    }
}
