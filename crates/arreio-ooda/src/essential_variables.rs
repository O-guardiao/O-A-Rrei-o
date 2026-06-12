use serde::{Deserialize, Serialize};

/// Variável essencial com limites inferior e superior (homeostase Ashby).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundedVariable<T> {
    pub min: T,
    pub max: T,
    pub current: T,
}

/// Conjunto de variáveis essenciais que definem a viabilidade do loop OODA-C.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EssentialVariables {
    pub eir: BoundedVariable<f64>,
    pub confidence: BoundedVariable<f64>,
    pub token_budget: BoundedVariable<u64>,
    pub latency_ms: BoundedVariable<u64>,
}

/// Snapshot imutável das variáveis essenciais em um instante.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EssentialSnapshot {
    pub eir: f64,
    pub confidence: f64,
    pub token_budget: u64,
    pub latency_ms: u64,
}

impl EssentialVariables {
    /// Constrói as variáveis essenciais a partir de tuplas (min, max, current).
    pub fn new(
        eir: (f64, f64, f64),
        confidence: (f64, f64, f64),
        token_budget: (u64, u64, u64),
        latency_ms: (u64, u64, u64),
    ) -> Self {
        Self {
            eir: BoundedVariable {
                min: eir.0,
                max: eir.1,
                current: eir.2,
            },
            confidence: BoundedVariable {
                min: confidence.0,
                max: confidence.1,
                current: confidence.2,
            },
            token_budget: BoundedVariable {
                min: token_budget.0,
                max: token_budget.1,
                current: token_budget.2,
            },
            latency_ms: BoundedVariable {
                min: latency_ms.0,
                max: latency_ms.1,
                current: latency_ms.2,
            },
        }
    }

    /// Retorna `true` se alguma variável estiver fora dos limites homeostáticos.
    pub fn any_variable_exceeded(&self) -> bool {
        self.eir.current > self.eir.max
            || self.eir.current < self.eir.min
            || self.confidence.current > self.confidence.max
            || self.confidence.current < self.confidence.min
            || self.token_budget.current > self.token_budget.max
            || self.token_budget.current < self.token_budget.min
            || self.latency_ms.current > self.latency_ms.max
            || self.latency_ms.current < self.latency_ms.min
    }

    /// Captura o estado atual em um snapshot imutável.
    pub fn snapshot(&self) -> EssentialSnapshot {
        EssentialSnapshot {
            eir: self.eir.current,
            confidence: self.confidence.current,
            token_budget: self.token_budget.current,
            latency_ms: self.latency_ms.current,
        }
    }

    /// Restaura os valores para os padrões homeostáticos (estável).
    pub fn restore_defaults(&mut self) {
        self.eir.current = 0.0;
        self.confidence.current = 1.0;
        self.token_budget.current = 0;
        self.latency_ms.current = 0;
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_variable_exceeded_when_within_bounds() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.5),
            (0.0, 1.0, 0.5),
            (0, 100, 50),
            (0, 1000, 500),
        );
        assert!(!vars.any_variable_exceeded());
    }

    #[test]
    fn any_variable_exceeded_when_out_of_bounds() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 1.5), // eir > max
            (0.0, 1.0, 0.5),
            (0, 100, 50),
            (0, 1000, 500),
        );
        assert!(vars.any_variable_exceeded());
    }

    #[test]
    fn snapshot_returns_current_values() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.1),
            (0.0, 1.0, 0.2),
            (0, 100, 30),
            (0, 1000, 250),
        );
        let snap = vars.snapshot();
        assert!((snap.eir - 0.1).abs() < f64::EPSILON);
        assert!((snap.confidence - 0.2).abs() < f64::EPSILON);
        assert_eq!(snap.token_budget, 30);
        assert_eq!(snap.latency_ms, 250);
    }

    #[test]
    fn restore_defaults_resets_variables() {
        let mut vars = EssentialVariables::new(
            (0.0, 1.0, 0.9),
            (0.0, 1.0, 0.1),
            (0, 100, 99),
            (0, 1000, 999),
        );
        vars.restore_defaults();
        assert!((vars.eir.current - 0.0).abs() < f64::EPSILON);
        assert!((vars.confidence.current - 1.0).abs() < f64::EPSILON);
        assert_eq!(vars.token_budget.current, 0);
        assert_eq!(vars.latency_ms.current, 0);
    }
}
