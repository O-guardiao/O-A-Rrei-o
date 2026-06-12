use anyhow::{bail, Result};

use crate::essential_variables::EssentialVariables;
use crate::igc::OrientationModel;

/// Fases do ciclo OODA-C.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Observe,
    Orient,
    Decide,
    Act,
}

/// Resultado de uma iteração do loop OODA-C.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionResult {
    pub phase: Phase,
    pub action: String,
    pub completed: bool,
    pub stability_reached: bool,
}

/// Motor principal do ciclo OODA-C com homeostase artificial.
pub struct OodacLoop {
    pub current_phase: Phase,
    pub orientation_model: OrientationModel,
    pub igc_enabled: bool,
    pub temperature: f64,
    pub reasoning_depth: String,
    pub conservative_mode: bool,
    pub essential_variables: Option<EssentialVariables>,
    pub eir_history: Vec<f64>,
    pub accuracy: f64,
    pub iterations: usize,
    pub last_action_result: Option<ActionResult>,
    pub max_act_latency_ms: u64,
}

impl OodacLoop {
    /// Cria um novo loop na fase Observe.
    pub fn new(orientation_model: OrientationModel) -> Self {
        Self {
            current_phase: Phase::Observe,
            orientation_model,
            igc_enabled: true,
            temperature: 0.7,
            reasoning_depth: "standard".to_string(),
            conservative_mode: false,
            essential_variables: None,
            eir_history: Vec::new(),
            accuracy: 0.8,
            iterations: 0,
            last_action_result: None,
            max_act_latency_ms: 5000,
        }
    }

    /// Injeta variáveis essenciais no loop.
    pub fn with_essential_variables(mut self, vars: EssentialVariables) -> Self {
        self.essential_variables = Some(vars);
        self
    }

    /// Define a acurácia esperada para o cálculo de estabilidade.
    pub fn with_accuracy(mut self, accuracy: f64) -> Self {
        self.accuracy = accuracy;
        self
    }

    /// Executa um ciclo completo OODA-C.
    ///
    /// Ciclo: Observe → Orient → (IG&C check) → Decide → Act → feedback loop.
    /// Após Act, verifica se o sistema atingiu estabilidade pela fórmula
    /// ECR/EIR ≤ Acc/(1−Acc).
    pub fn run_cycle(&mut self, input: &str) -> Result<ActionResult> {
        // ── Observe ─────────────────────────────────────────────────────────────
        self.current_phase = Phase::Observe;

        // ── Orient ──────────────────────────────────────────────────────────────
        self.current_phase = Phase::Orient;

        // ── IG&C check ──────────────────────────────────────────────────────────
        let bypass_decide = self.igc_enabled && self.orientation_model.should_bypass_decide();

        if !bypass_decide {
            self.current_phase = Phase::Decide;
        }

        // ── Act ─────────────────────────────────────────────────────────────────
        self.current_phase = Phase::Act;

        // Verifica timeout de latência no Act.
        if let Some(ref vars) = self.essential_variables {
            if vars.latency_ms.current > self.max_act_latency_ms {
                bail!(
                    "timeout no Act: latência {}ms excede {}ms",
                    vars.latency_ms.current,
                    self.max_act_latency_ms
                );
            }
        }

        // Atualiza histórico e contadores.
        self.iterations += 1;
        if let Some(ref vars) = self.essential_variables {
            self.eir_history.push(vars.eir.current);
        }

        // Step-function trigger (ultrastabilidade Ashby).
        crate::step_function::reparametrize(self);

        // Verifica estabilidade: iteração para quando ECR/EIR ≤ Acc/(1−Acc).
        let stability_reached = self.check_stability();

        let result = ActionResult {
            phase: Phase::Act,
            action: input.to_string(),
            completed: true,
            stability_reached,
        };

        // Feedback loop: resultado alimenta próxima observação.
        self.last_action_result = Some(result.clone());
        self.current_phase = Phase::Observe;

        Ok(result)
    }

    /// Verifica se o sistema atingiu estabilidade homeostática.
    ///
    /// Fórmula: para quando ECR/EIR ≤ Acc/(1−Acc).
    /// ECR (Error Correction Rate) é aproximado como 1.0 − EIR.
    fn check_stability(&self) -> bool {
        let Some(ref vars) = self.essential_variables else {
            return false;
        };

        let eir = vars.eir.current;
        if eir <= 0.0 {
            return true; // sem erros → estável
        }

        let ecr = 1.0 - eir;
        let ratio = ecr / eir;

        let threshold = if self.accuracy >= 1.0 {
            f64::MAX
        } else {
            self.accuracy / (1.0 - self.accuracy)
        };

        ratio <= threshold
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::essential_variables::EssentialVariables;
    use crate::igc::OrientationModel;

    fn default_orientation() -> OrientationModel {
        OrientationModel {
            confidence: 0.5,
            task_type: "test".to_string(),
            implicit_operator: "identity".to_string(),
            strategy: "standard".to_string(),
        }
    }

    #[test]
    fn initial_phase_is_observe() {
        let loop_ = OodacLoop::new(default_orientation());
        assert_eq!(loop_.current_phase, Phase::Observe);
    }

    #[test]
    fn phase_transition_observe_to_act() {
        let mut loop_ = OodacLoop::new(default_orientation());
        let result = loop_.run_cycle("test_input").unwrap();
        assert_eq!(loop_.iterations, 1);
        assert!(result.completed);
    }

    #[test]
    fn cycle_with_igc_bypass() {
        let mut model = default_orientation();
        model.confidence = 0.95;
        let mut loop_ = OodacLoop::new(model);
        let result = loop_.run_cycle("fast_action").unwrap();
        assert!(result.completed);
        assert_eq!(loop_.iterations, 1);
    }

    #[test]
    fn feedback_loop_feeds_next_observation() {
        let mut loop_ = OodacLoop::new(default_orientation());
        let _r1 = loop_.run_cycle("first").unwrap();
        assert!(loop_.last_action_result.is_some());
        assert_eq!(loop_.last_action_result.as_ref().unwrap().action, "first");

        let _r2 = loop_.run_cycle("second").unwrap();
        assert_eq!(loop_.last_action_result.as_ref().unwrap().action, "second");
        assert_eq!(loop_.iterations, 2);
    }

    #[test]
    fn stability_stops_when_ratio_below_threshold() {
        // accuracy = 0.5 → threshold = 1.0
        // eir = 0.6 → ecr = 0.4 → ratio = 0.67 ≤ 1.0 → estável
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.6), // eir
            (0.0, 1.0, 0.5), // confidence
            (0, 1000, 0),    // token_budget
            (0, 5000, 100),  // latency_ms
        );
        let mut loop_ = OodacLoop::new(default_orientation())
            .with_essential_variables(vars)
            .with_accuracy(0.5);
        let result = loop_.run_cycle("stable_input").unwrap();
        assert!(result.stability_reached);
    }

    #[test]
    fn act_timeout_5000ms() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.01),
            (0.0, 1.0, 0.9),
            (0, 1000, 0),
            (0, 5000, 6000), // latência acima de 5000ms
        );
        let mut loop_ = OodacLoop::new(default_orientation()).with_essential_variables(vars);
        let result = loop_.run_cycle("slow_action");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timeout"));
    }

    #[test]
    fn full_cycle_with_mock_input() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.01),
            (0.0, 1.0, 0.8),
            (0, 1000, 10),
            (0, 5000, 50),
        );
        let mut loop_ = OodacLoop::new(default_orientation()).with_essential_variables(vars);
        let result = loop_.run_cycle("mock_input").unwrap();
        assert_eq!(result.action, "mock_input");
        assert!(result.completed);
        assert_eq!(loop_.current_phase, Phase::Observe);
    }
}
