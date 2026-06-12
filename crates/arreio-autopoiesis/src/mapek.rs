use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::health_monitor::HealthMonitor;

/// Ação corretiva gerada pelo loop MAPE-K.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Action {
    AdjustParameter { name: String, value: f64 },
    ReconfigureSubsystem { name: String, config: String },
    RestartService { name: String },
    Escalate { reason: String },
}

/// Plano composto por zero ou mais ações.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    pub actions: Vec<Action>,
}

/// Base de conhecimento: histórico de estados e ações executadas.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KnowledgeBase {
    pub history: Vec<(String, f64)>, // (nome_variável, valor)
    pub actions_taken: Vec<Action>,
}

impl KnowledgeBase {
    /// Cria uma knowledge base vazia.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registra uma leitura de variável no histórico.
    pub fn record(&mut self, name: &str, value: f64) {
        self.history.push((name.to_string(), value));
    }

    /// Registra uma ação executada.
    pub fn record_action(&mut self, action: Action) {
        self.actions_taken.push(action);
    }
}

/// Tendência detectada na análise de uma variável.
#[derive(Debug, Clone, PartialEq)]
pub enum Trend {
    Improving,
    Stable,
    Deteriorating,
}

/// Fase de Monitoramento do MAPE-K.
pub struct Monitor;

/// Fase de Análise do MAPE-K — detecta tendências.
pub struct Analyze;

impl Analyze {
    /// Detecta a tendência de uma variável a partir do histórico.
    ///
    /// Compara os dois últimos valores: aumento = deterioração,
    /// diminuição = melhoria, igual = estável.
    pub fn detect_trend(history: &[(String, f64)], variable_name: &str) -> Trend {
        let values: Vec<f64> = history
            .iter()
            .filter(|(n, _)| n == variable_name)
            .map(|(_, v)| *v)
            .collect();

        if values.len() < 2 {
            return Trend::Stable;
        }

        let last = values[values.len() - 1];
        let prev = values[values.len() - 2];

        if last > prev {
            Trend::Deteriorating
        } else if last < prev {
            Trend::Improving
        } else {
            Trend::Stable
        }
    }
}

/// Fase de Planejamento do MAPE-K — gera ações corretivas.
pub struct PlanGenerator;

impl PlanGenerator {
    /// Gera um plano de ações baseado no estado atual do monitor e no histórico.
    pub fn generate(monitor: &HealthMonitor, knowledge: &KnowledgeBase) -> Plan {
        let mut actions = Vec::new();

        for var in &monitor.variables {
            if !var.is_healthy() {
                let trend = Analyze::detect_trend(&knowledge.history, &var.name);

                // Se está melhorando, apenas observa; caso contrário, age.
                if trend == Trend::Improving {
                    continue;
                }

                match var.name.as_str() {
                    "latency_ms" => {
                        actions.push(Action::AdjustParameter {
                            name: "batch_size".to_string(),
                            value: 0.5,
                        });
                    }
                    "error_rate" => {
                        actions.push(Action::ReconfigureSubsystem {
                            name: "validator".to_string(),
                            config: "strict".to_string(),
                        });
                    }
                    "memory_usage" => {
                        actions.push(Action::RestartService {
                            name: "worker".to_string(),
                        });
                    }
                    "token_consumption" => {
                        actions.push(Action::AdjustParameter {
                            name: "token_limit".to_string(),
                            value: var.threshold_max * 0.8,
                        });
                    }
                    _ => {
                        actions.push(Action::Escalate {
                            reason: format!("{} fora do limite (tendência: {:?})", var.name, trend),
                        });
                    }
                }
            }
        }

        Plan { actions }
    }
}

/// Fase de Execução do MAPE-K.
pub struct Execute;

/// Loop MAPE-K completo: Monitor → Analyze → Plan → Execute → Knowledge.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MapekLoop {
    pub knowledge: KnowledgeBase,
    pub last_plan: Option<Plan>,
}

impl MapekLoop {
    /// Cria um novo loop MAPE-K com knowledge base vazia.
    pub fn new() -> Self {
        Self::default()
    }

    /// Executa um ciclo completo MAPE-K.
    ///
    /// 1. Monitor: coleta variáveis de saúde para o knowledge.
    /// 2. Analyze + Plan: detecta tendências e gera ações corretivas.
    /// 3. Execute: registra ações no knowledge.
    pub fn run_cycle(&mut self, monitor: &HealthMonitor) -> Result<Vec<Action>> {
        // Monitor
        for var in &monitor.variables {
            self.knowledge.record(&var.name, var.value);
        }

        // Analyze + Plan
        let plan = PlanGenerator::generate(monitor, &self.knowledge);
        let actions = plan.actions.clone();

        // Execute (registra no knowledge)
        for action in &actions {
            self.knowledge.record_action(action.clone());
        }

        self.last_plan = Some(plan);
        Ok(actions)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_monitor::{HealthMonitor, HealthVariable};

    #[test]
    fn ciclo_completo_retorna_acoes() {
        let mut monitor = HealthMonitor::new();
        monitor.register(HealthVariable {
            name: "latency_ms".to_string(),
            value: 1500.0,
            threshold_min: 0.0,
            threshold_max: 1000.0,
        });

        let mut mapek = MapekLoop::new();
        let actions = mapek.run_cycle(&monitor).unwrap();

        assert!(!actions.is_empty());
        assert_eq!(mapek.knowledge.history.len(), 1);
    }

    #[test]
    fn analyze_detecta_deterioracao() {
        let history = vec![
            ("latency_ms".to_string(), 100.0),
            ("latency_ms".to_string(), 200.0),
        ];
        assert_eq!(
            Analyze::detect_trend(&history, "latency_ms"),
            Trend::Deteriorating
        );
    }

    #[test]
    fn analyze_detecta_melhoria() {
        let history = vec![
            ("error_rate".to_string(), 0.1),
            ("error_rate".to_string(), 0.05),
        ];
        assert_eq!(
            Analyze::detect_trend(&history, "error_rate"),
            Trend::Improving
        );
    }

    #[test]
    fn analyze_detecta_estavel() {
        let history = vec![
            ("memory_usage".to_string(), 0.5),
            ("memory_usage".to_string(), 0.5),
        ];
        assert_eq!(
            Analyze::detect_trend(&history, "memory_usage"),
            Trend::Stable
        );
    }

    #[test]
    fn sistema_saudavel_nao_gera_acoes() {
        let monitor = HealthMonitor::with_defaults();
        let mut mapek = MapekLoop::new();
        let actions = mapek.run_cycle(&monitor).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn sistema_doente_gera_multiplas_acoes() {
        let mut monitor = HealthMonitor::with_defaults();
        monitor.update("latency_ms", 2000.0).unwrap();
        monitor.update("error_rate", 0.1).unwrap();
        monitor.update("memory_usage", 0.95).unwrap();

        let mut mapek = MapekLoop::new();
        let actions = mapek.run_cycle(&monitor).unwrap();

        assert_eq!(actions.len(), 3);
    }

    #[test]
    fn knowledge_guarda_historico() {
        let mut knowledge = KnowledgeBase::new();
        knowledge.record("cpu", 0.5);
        knowledge.record("cpu", 0.6);
        assert_eq!(knowledge.history.len(), 2);
    }
}
