use anyhow::Result;
use arreio_kernel::blackboard::Blackboard;
use serde_json;

use crate::mapek::Action;

/// Resultado da aplicação de uma ação de auto-cura.
#[derive(Debug, Clone, PartialEq)]
pub struct HealingResult {
    pub action: Action,
    pub success: bool,
    pub before: f64,
    pub after: f64,
}

/// Sistema de auto-cura que aplica ações corretivas ao ecossistema.
///
/// Pode ser vinculado a um Blackboard para persistir mudanças de configuração
/// e publicar alertas.
pub struct SelfHealing {
    pub blackboard: Option<Blackboard>,
}

impl SelfHealing {
    pub fn new() -> Self {
        Self { blackboard: None }
    }

    pub fn with_blackboard(blackboard: Blackboard) -> Self {
        Self {
            blackboard: Some(blackboard),
        }
    }

    /// Aplica uma lista de ações corretivas e retorna os resultados.
    ///
    /// Ações implementadas:
    /// - `AdjustParameter`: atualiza config no Blackboard (`autopoiesis:config:*`).
    /// - `ReconfigureSubsystem`: atualiza parâmetro de subsistema no Blackboard.
    /// - `RestartService`: registra solicitação de reinício no Blackboard.
    /// - `Escalate`: publica alerta no Blackboard no tópico `autopoiesis:alert`.
    pub fn heal(&mut self, actions: Vec<Action>) -> Result<Vec<HealingResult>> {
        let mut results = Vec::new();

        for action in actions {
            let result = match &action {
                Action::AdjustParameter { name, value } => {
                    if let Some(ref bb) = self.blackboard {
                        bb.put_tuple("autopoiesis:config", name, serde_json::json!(value))?;
                    }
                    HealingResult {
                        action: action.clone(),
                        success: true,
                        before: 0.0,
                        after: *value,
                    }
                }
                Action::ReconfigureSubsystem { name, config } => {
                    // Reconfiguração real: persiste a nova configuração no Blackboard
                    let success = if let Some(ref bb) = self.blackboard {
                        bb.put_tuple(
                            "autopoiesis:reconfig",
                            name,
                            serde_json::json!({ "config": config, "timestamp": now() }),
                        )
                        .is_ok()
                    } else {
                        true
                    };
                    HealingResult {
                        action: action.clone(),
                        success,
                        before: 0.0,
                        after: 1.0,
                    }
                }
                Action::RestartService { name } => {
                    // Reinício real: publica sinal de reinício no Blackboard
                    // O loop principal deve reagir a este sinal na próxima iteração
                    let success = if let Some(ref bb) = self.blackboard {
                        bb.put_tuple(
                            "autopoiesis:restart",
                            name,
                            serde_json::json!({ "requested_at": now(), "status": "pending" }),
                        )
                        .is_ok()
                    } else {
                        true
                    };
                    HealingResult {
                        action: action.clone(),
                        success,
                        before: 0.0,
                        after: 1.0,
                    }
                }
                Action::Escalate { reason } => {
                    if let Some(ref bb) = self.blackboard {
                        bb.publish(
                            "autopoiesis:alert",
                            serde_json::json!({
                                "reason": reason,
                                "level": "critical",
                                "timestamp": now()
                            }),
                        )?;
                    }
                    HealingResult {
                        action: action.clone(),
                        success: true,
                        before: 0.0,
                        after: 1.0,
                    }
                }
            };
            results.push(result);
        }

        Ok(results)
    }
}

impl Default for SelfHealing {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::blackboard::Blackboard;
    use tempfile::NamedTempFile;

    fn temp_blackboard() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    #[test]
    fn aplica_adjust_parameter_sem_blackboard() {
        let mut healing = SelfHealing::new();
        let actions = vec![Action::AdjustParameter {
            name: "batch_size".to_string(),
            value: 0.5,
        }];
        let results = healing.heal(actions).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert!((results[0].after - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn aplica_adjust_parameter_com_blackboard() {
        let bb = temp_blackboard();
        let mut healing = SelfHealing::with_blackboard(bb.clone());
        let actions = vec![Action::AdjustParameter {
            name: "token_limit".to_string(),
            value: 8000.0,
        }];
        let results = healing.heal(actions).unwrap();
        assert!(results[0].success);

        let val = bb.get_tuple("autopoiesis:config", "token_limit");
        assert!(val.is_some());
        assert!((val.unwrap().as_f64().unwrap() - 8000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn publica_alerta_no_blackboard() {
        let bb = temp_blackboard();
        let mut healing = SelfHealing::with_blackboard(bb.clone());
        let actions = vec![Action::Escalate {
            reason: "latência crítica".to_string(),
        }];
        let results = healing.heal(actions).unwrap();
        assert!(results[0].success);

        let event = bb.next_event("autopoiesis:alert");
        assert!(event.is_some());
        assert_eq!(event.unwrap().data["reason"], "latência crítica");
    }

    #[test]
    fn reconfigura_subsistema_real() {
        let bb = temp_blackboard();
        let mut healing = SelfHealing::with_blackboard(bb.clone());
        let actions = vec![Action::ReconfigureSubsystem {
            name: "validator".to_string(),
            config: "strict".to_string(),
        }];
        let results = healing.heal(actions).unwrap();
        assert!(results[0].success);

        let val = bb.get_tuple("autopoiesis:reconfig", "validator");
        assert!(val.is_some());
        assert_eq!(val.unwrap()["config"], "strict");
    }

    #[test]
    fn restart_service_real() {
        let bb = temp_blackboard();
        let mut healing = SelfHealing::with_blackboard(bb.clone());
        let actions = vec![Action::RestartService {
            name: "worker".to_string(),
        }];
        let results = healing.heal(actions).unwrap();
        assert!(results[0].success);

        let val = bb.get_tuple("autopoiesis:restart", "worker");
        assert!(val.is_some());
        assert_eq!(val.unwrap()["status"], "pending");
    }
}
