use crate::loop_engine::OodacLoop;

/// Reparametrização discreta do loop OODA-C (ultrastabilidade Ashby).
///
/// Triggers:
/// - EIR > 0.05 por 3 ciclos consecutivos → temperature = 0.3, igc_enabled = false,
///   reasoning_depth = "tot_3", restaura variáveis essenciais.
/// - confidence < 0.5 → ativa modo conservador (temperature = 0.1).
pub fn reparametrize(loop_state: &mut OodacLoop) {
    // Trigger por EIR alto em 3 ciclos consecutivos.
    let consecutive_high_eir = loop_state
        .eir_history
        .iter()
        .rev()
        .take_while(|&&v| v > 0.05)
        .count();

    if consecutive_high_eir >= 3 {
        loop_state.temperature = 0.3;
        loop_state.igc_enabled = false;
        loop_state.reasoning_depth = "tot_3".to_string();
        if let Some(ref mut vars) = loop_state.essential_variables {
            vars.restore_defaults();
        }
        // Limpa histórico para evitar reparametrização contínua.
        loop_state.eir_history.clear();
    }

    // Trigger por confidence baixa.
    if let Some(ref vars) = loop_state.essential_variables {
        if vars.confidence.current < 0.5 {
            loop_state.conservative_mode = true;
            loop_state.temperature = 0.1;
        }
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
    fn step_function_trigger_high_eir() {
        let mut loop_ = OodacLoop::new(default_orientation());
        // Simula 3 ciclos consecutivos com EIR alto.
        loop_.eir_history = vec![0.1, 0.08, 0.06];
        reparametrize(&mut loop_);
        assert!((loop_.temperature - 0.3).abs() < f64::EPSILON);
        assert!(!loop_.igc_enabled);
        assert_eq!(loop_.reasoning_depth, "tot_3");
    }

    #[test]
    fn step_function_trigger_low_confidence() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.01),
            (0.0, 1.0, 0.3), // confidence baixa
            (0, 1000, 0),
            (0, 5000, 100),
        );
        let mut loop_ = OodacLoop::new(default_orientation()).with_essential_variables(vars);
        reparametrize(&mut loop_);
        assert!(loop_.conservative_mode);
        assert!((loop_.temperature - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn reparametrization_restores_essential_variables() {
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.9),
            (0.0, 1.0, 0.1),
            (0, 1000, 999),
            (0, 5000, 6000),
        );
        let mut loop_ = OodacLoop::new(default_orientation()).with_essential_variables(vars);
        loop_.eir_history = vec![0.1, 0.08, 0.06];
        reparametrize(&mut loop_);
        let restored = loop_.essential_variables.as_ref().unwrap();
        assert!((restored.eir.current - 0.0).abs() < f64::EPSILON);
        assert!((restored.confidence.current - 1.0).abs() < f64::EPSILON);
        assert_eq!(restored.token_budget.current, 0);
        assert_eq!(restored.latency_ms.current, 0);
    }
}
