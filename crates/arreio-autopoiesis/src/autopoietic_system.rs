use anyhow::Result;
use arreio_kernel::blackboard::Blackboard;
use serde_json;

use crate::health_monitor::HealthMonitor;
use crate::mapek::{Action, MapekLoop};
use crate::self_healing::SelfHealing;

/// Resultado de um ciclo completo do sistema autopoietico.
#[derive(Debug, Clone, PartialEq)]
pub struct TickResult {
    /// Indica se todas as variáveis de saúde estão dentro dos limites.
    pub healthy: bool,
    /// Ações corretivas planejadas e aplicadas neste tick.
    pub actions_taken: Vec<Action>,
    /// Alertas de variáveis fora dos limites.
    pub alerts: Vec<String>,
}

/// Sistema autopoietico integrado: monitora, analisa, planeja, executa e cura.
///
/// Combina HealthMonitor + MAPE-K + SelfHealing em um único ciclo `tick()`.
/// Pode ser usado como monitor de variáveis essenciais pelo OODA-C Layer 0.
pub struct AutopoieticSystem {
    pub monitor: HealthMonitor,
    pub mapek: MapekLoop,
    pub healing: SelfHealing,
    pub blackboard: Option<Blackboard>,
}

impl AutopoieticSystem {
    /// Cria um sistema autopoietico com monitor padrão.
    pub fn new() -> Self {
        Self {
            monitor: HealthMonitor::with_defaults(),
            mapek: MapekLoop::new(),
            healing: SelfHealing::new(),
            blackboard: None,
        }
    }

    /// Vincula o sistema a um Blackboard para persistência e alertas.
    pub fn with_blackboard(mut self, blackboard: Blackboard) -> Self {
        self.blackboard = Some(blackboard.clone());
        self.healing = SelfHealing::with_blackboard(blackboard);
        self
    }

    /// Executa um ciclo completo autopoietico.
    ///
    /// Fluxo: monitor → analyze → plan → execute → knowledge update.
    /// Persiste o estado de saúde no Blackboard sob `autopoiesis:health:*`.
    pub fn tick(&mut self) -> Result<TickResult> {
        // Persiste estado de saúde no Blackboard.
        if let Some(ref bb) = self.blackboard {
            for var in &self.monitor.variables {
                bb.put_tuple(
                    "autopoiesis:health",
                    &var.name,
                    serde_json::json!({
                        "value": var.value,
                        "threshold_min": var.threshold_min,
                        "threshold_max": var.threshold_max,
                        "healthy": var.is_healthy(),
                    }),
                )?;
            }
        }

        let healthy = self.monitor.is_healthy();
        let alerts = self.monitor.alerts();

        // Ciclo MAPE-K: monitor → analyze → plan.
        let actions = self.mapek.run_cycle(&self.monitor)?;

        // Auto-cura: execute.
        let healing_results = self.healing.heal(actions.clone())?;

        // Atualiza knowledge com resultados do healing.
        for result in &healing_results {
            if result.success {
                self.mapek.knowledge.record_action(result.action.clone());
            }
        }

        Ok(TickResult {
            healthy,
            actions_taken: actions,
            alerts,
        })
    }
}

impl Default for AutopoieticSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

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
    fn tick_completo_sistema_saudavel() {
        let bb = temp_blackboard();
        let mut system = AutopoieticSystem::new().with_blackboard(bb);
        let result = system.tick().unwrap();

        assert!(result.healthy);
        assert!(result.actions_taken.is_empty());
        assert!(result.alerts.is_empty());
    }

    #[test]
    fn tick_completo_sistema_doente() {
        let bb = temp_blackboard();
        let mut system = AutopoieticSystem::new().with_blackboard(bb);
        system.monitor.update("latency_ms", 2000.0).unwrap();
        system.monitor.update("error_rate", 0.1).unwrap();

        let result = system.tick().unwrap();

        assert!(!result.healthy);
        assert!(!result.actions_taken.is_empty());
        assert!(!result.alerts.is_empty());
    }

    #[test]
    fn sistema_saudavel_nao_gera_acoes() {
        let mut system = AutopoieticSystem::new();
        let result = system.tick().unwrap();
        assert!(result.healthy);
        assert!(result.actions_taken.is_empty());
    }

    #[test]
    fn sistema_doente_gera_multiplas_acoes() {
        let mut system = AutopoieticSystem::new();
        system.monitor.update("latency_ms", 2000.0).unwrap();
        system.monitor.update("error_rate", 0.1).unwrap();
        system.monitor.update("memory_usage", 0.95).unwrap();
        system.monitor.update("token_consumption", 15000.0).unwrap();

        let result = system.tick().unwrap();
        assert!(!result.healthy);
        assert_eq!(result.actions_taken.len(), 4);
    }

    #[test]
    fn persiste_estado_saude_no_blackboard() {
        let bb = temp_blackboard();
        let mut system = AutopoieticSystem::new().with_blackboard(bb.clone());
        system.monitor.update("latency_ms", 500.0).unwrap();

        let _ = system.tick().unwrap();

        let val = bb.get_tuple("autopoiesis:health", "latency_ms");
        assert!(val.is_some());
        let data = val.unwrap();
        assert_eq!(data["value"], 500.0);
        assert_eq!(data["healthy"], true);
    }

    #[test]
    fn tick_result_contem_alerts_corretos() {
        let mut system = AutopoieticSystem::new();
        system.monitor.update("latency_ms", 2000.0).unwrap();
        let result = system.tick().unwrap();

        assert!(!result.healthy);
        assert_eq!(result.alerts.len(), 1);
        assert!(result.alerts[0].contains("latency_ms"));
    }

    #[test]
    fn knowledge_atualiza_apos_tick() {
        let mut system = AutopoieticSystem::new();
        system.monitor.update("error_rate", 0.1).unwrap();

        let _ = system.tick().unwrap();
        // O knowledge deve conter o registro da variável + a ação executada.
        assert!(!system.mapek.knowledge.history.is_empty());
    }
}
