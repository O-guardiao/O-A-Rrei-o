use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Variável de saúde monitorada com limites aceitáveis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthVariable {
    pub name: String,
    pub value: f64,
    pub threshold_min: f64,
    pub threshold_max: f64,
}

impl HealthVariable {
    /// Retorna true se o valor está dentro dos thresholds.
    pub fn is_healthy(&self) -> bool {
        self.value >= self.threshold_min && self.value <= self.threshold_max
    }
}

/// Monitor de saúde que acompanha múltiplas variáveis do ecossistema.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HealthMonitor {
    pub variables: Vec<HealthVariable>,
}

impl HealthMonitor {
    /// Cria um monitor vazio.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cria um monitor com as variáveis padrão do sistema.
    pub fn with_defaults() -> Self {
        let mut monitor = Self::new();
        monitor.register(HealthVariable {
            name: "latency_ms".to_string(),
            value: 0.0,
            threshold_min: 0.0,
            threshold_max: 1000.0,
        });
        monitor.register(HealthVariable {
            name: "error_rate".to_string(),
            value: 0.0,
            threshold_min: 0.0,
            threshold_max: 0.05,
        });
        monitor.register(HealthVariable {
            name: "token_consumption".to_string(),
            value: 0.0,
            threshold_min: 0.0,
            threshold_max: 10000.0,
        });
        monitor.register(HealthVariable {
            name: "memory_usage".to_string(),
            value: 0.0,
            threshold_min: 0.0,
            threshold_max: 0.9,
        });
        monitor
    }

    /// Registra uma nova variável no monitor.
    pub fn register(&mut self, var: HealthVariable) {
        self.variables.push(var);
    }

    /// Atualiza o valor de uma variável existente.
    pub fn update(&mut self, name: &str, value: f64) -> Result<()> {
        let var = self.variables.iter_mut().find(|v| v.name == name);
        match var {
            Some(v) => {
                v.value = value;
                Ok(())
            }
            None => bail!("variável de saúde '{}' não encontrada", name),
        }
    }

    /// Retorna true se todas as variáveis estão saudáveis.
    pub fn is_healthy(&self) -> bool {
        self.variables.iter().all(|v| v.is_healthy())
    }

    /// Lista alertas para variáveis fora dos limites.
    pub fn alerts(&self) -> Vec<String> {
        self.variables
            .iter()
            .filter(|v| !v.is_healthy())
            .map(|v| {
                format!(
                    "{} = {} (fora de [{}, {}])",
                    v.name, v.value, v.threshold_min, v.threshold_max
                )
            })
            .collect()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registra_e_atualiza_variavel() {
        let mut monitor = HealthMonitor::new();
        monitor.register(HealthVariable {
            name: "cpu".to_string(),
            value: 0.0,
            threshold_min: 0.0,
            threshold_max: 1.0,
        });
        monitor.update("cpu", 0.5).unwrap();
        assert!((monitor.variables[0].value - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn detecta_variavel_fora_do_limite() {
        let mut monitor = HealthMonitor::with_defaults();
        monitor.update("latency_ms", 1500.0).unwrap();
        assert!(!monitor.is_healthy());
        let alerts = monitor.alerts();
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("latency_ms"));
    }

    #[test]
    fn is_healthy_quando_todas_ok() {
        let monitor = HealthMonitor::with_defaults();
        assert!(monitor.is_healthy());
        assert!(monitor.alerts().is_empty());
    }

    #[test]
    fn alerts_retorna_multiplas_variaveis_fora() {
        let mut monitor = HealthMonitor::with_defaults();
        monitor.update("latency_ms", 2000.0).unwrap();
        monitor.update("error_rate", 0.1).unwrap();
        let alerts = monitor.alerts();
        assert_eq!(alerts.len(), 2);
    }

    #[test]
    fn update_variavel_inexistente_retorna_erro() {
        let mut monitor = HealthMonitor::new();
        let err = monitor.update("inexistente", 1.0);
        assert!(err.is_err());
    }
}
