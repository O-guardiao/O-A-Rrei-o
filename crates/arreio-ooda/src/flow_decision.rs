//! Decisão de fluxo do pipeline SYMBION baseada no estado do OODA-C.
//!
//! O `OodacLoop` usa as variáveis essenciais e o classificador de padrões
//! para decidir quais camadas do pipeline devem ser ativadas.
//! Isso transforma o loop de orquestração rígida em um sistema de
//! controle adaptativo que escala recursos computacionais conforme a
//! incerteza da tarefa.

use crate::essential_variables::EssentialVariables;
use crate::pattern_classifier::PatternClassifier;

/// Camadas do pipeline SYMBION que podem ser ativadas/desativadas.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowDecision {
    /// Executa ProblemSpace decomposition.
    pub problem_space: bool,
    /// Executa Meta-Cognitive risk assessment.
    pub meta_cognitive: bool,
    /// Executa Refinement-Based Generation.
    pub refinement: bool,
    /// Executa Recovery Block Multi-Model.
    pub recovery: bool,
    /// Executa Contract Verification.
    pub contract: bool,
    /// Executa Supercompilation / Equality Saturation.
    pub supercompile: bool,
    /// Executa Chunking Cache Update.
    pub chunking: bool,
    /// Executa Autopoietic Health Check.
    pub autopoiesis: bool,
    /// Justificativa da decisão (para audit logging).
    pub reason: String,
}

impl FlowDecision {
    /// Caminho mínimo: apenas execução direta (IG&C bypass total).
    /// Usado quando padrão é rotineiro e confiança é máxima.
    pub fn fast_path(reason: impl Into<String>) -> Self {
        Self {
            problem_space: true,
            meta_cognitive: false,
            refinement: false,
            recovery: true,
            contract: false,
            supercompile: false,
            chunking: true,
            autopoiesis: true,
            reason: reason.into(),
        }
    }

    /// Caminho padrão: todas as camadas exceto supercompilation condicional.
    pub fn standard(reason: impl Into<String>) -> Self {
        Self {
            problem_space: true,
            meta_cognitive: true,
            refinement: true,
            recovery: true,
            contract: true,
            supercompile: true,
            chunking: true,
            autopoiesis: true,
            reason: reason.into(),
        }
    }

    /// Caminho de alta incerteza: todas as camadas + conservador.
    pub fn deep_deliberation(reason: impl Into<String>) -> Self {
        Self {
            problem_space: true,
            meta_cognitive: true,
            refinement: true,
            recovery: true,
            contract: true,
            supercompile: true,
            chunking: true,
            autopoiesis: true,
            reason: reason.into(),
        }
    }

    /// Caminho de emergência: homeostase violada, executa apenas verificações.
    pub fn emergency(reason: impl Into<String>) -> Self {
        Self {
            problem_space: true,
            meta_cognitive: false,
            refinement: false,
            recovery: false,
            contract: false,
            supercompile: false,
            chunking: false,
            autopoiesis: true,
            reason: reason.into(),
        }
    }
}

/// Motor de decisão de fluxo.
pub struct FlowController {
    classifier: PatternClassifier,
}

impl FlowController {
    pub fn new() -> Self {
        Self {
            classifier: PatternClassifier::new(),
        }
    }

    /// Cria um controller com padrões pré-registrados.
    pub fn with_patterns(patterns: Vec<&str>) -> Self {
        Self {
            classifier: PatternClassifier::with_patterns(patterns),
        }
    }

    /// Registra um padrão rotineiro.
    pub fn register_pattern(&mut self, pattern: &str) {
        self.classifier.register(pattern);
    }

    /// Decide o fluxo com base no input e nas variáveis essenciais.
    pub fn decide(
        &self,
        input: &str,
        model_confidence: f64,
        vars: Option<&EssentialVariables>,
    ) -> FlowDecision {
        // 1. Verificação de emergência: variáveis essenciais fora dos limites.
        if let Some(v) = vars {
            if v.any_variable_exceeded() {
                return FlowDecision::emergency(format!(
                    "variável essencial excedida: eir={:.2}, confidence={:.2}, tokens={}, latency={}ms",
                    v.eir.current, v.confidence.current, v.token_budget.current, v.latency_ms.current
                ));
            }

            // 2. Se token budget está baixo, desativa camadas pesadas.
            let token_ratio = v.token_budget.current as f64 / v.token_budget.max as f64;
            if token_ratio > 0.8 {
                return FlowDecision::fast_path(format!(
                    "token budget crítico ({:.0}%): bypass de meta-cognitive, contract, supercompile",
                    token_ratio * 100.0
                ));
            }
        }

        // 3. IG&C bypass: padrão rotineiro + alta confiança.
        if self.classifier.should_bypass(input, model_confidence) {
            return FlowDecision::fast_path(format!(
                "IG&C bypass: padrão rotineiro match (confidence={:.2})",
                model_confidence
            ));
        }

        // 4. Confiança intermediária: caminho padrão.
        if model_confidence >= 0.6 {
            return FlowDecision::standard(format!(
                "caminho padrão: confidence={:.2} dentro da faixa normal",
                model_confidence
            ));
        }

        // 5. Baixa confiança: deep deliberation.
        FlowDecision::deep_deliberation(format!(
            "deep deliberation: confidence={:.2} abaixo do threshold",
            model_confidence
        ))
    }
}

impl Default for FlowController {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::essential_variables::EssentialVariables;

    #[test]
    fn fast_path_for_known_pattern_and_high_confidence() {
        let mut ctrl = FlowController::new();
        ctrl.register_pattern("hello world");

        let decision = ctrl.decide("hello world", 0.95, None);
        assert_eq!(decision.meta_cognitive, false);
        assert_eq!(decision.contract, false);
        assert_eq!(decision.supercompile, false);
        assert!(decision.reason.contains("IG&C bypass"));
    }

    #[test]
    fn standard_path_for_unknown_pattern() {
        let ctrl = FlowController::new();
        let decision = ctrl.decide("tarefa complexa desconhecida", 0.80, None);
        assert_eq!(decision.meta_cognitive, true);
        assert_eq!(decision.contract, true);
        assert_eq!(decision.supercompile, true);
        assert!(decision.reason.contains("padrão"));
    }

    #[test]
    fn deep_deliberation_for_low_confidence() {
        let ctrl = FlowController::new();
        let decision = ctrl.decide("tarefa", 0.30, None);
        assert_eq!(decision.meta_cognitive, true);
        assert_eq!(decision.refinement, true);
        assert!(decision.reason.contains("deep deliberation"));
    }

    #[test]
    fn emergency_when_variable_exceeded() {
        let ctrl = FlowController::new();
        let vars = EssentialVariables::new(
            (0.0, 1.0, 1.5), // eir > max
            (0.0, 1.0, 0.5),
            (0, 100, 50),
            (0, 1000, 500),
        );
        let decision = ctrl.decide("anything", 0.95, Some(&vars));
        assert_eq!(decision.recovery, false);
        assert_eq!(decision.contract, false);
        assert!(decision.reason.contains("variável essencial excedida"));
    }

    #[test]
    fn fast_path_when_token_budget_critical() {
        let ctrl = FlowController::new();
        let vars = EssentialVariables::new(
            (0.0, 1.0, 0.1),
            (0.0, 1.0, 0.8),
            (0, 100, 85), // 85% do budget
            (0, 1000, 100),
        );
        let decision = ctrl.decide("anything", 0.50, Some(&vars));
        assert_eq!(decision.meta_cognitive, false);
        assert_eq!(decision.contract, false);
        assert_eq!(decision.supercompile, false);
        assert!(decision.reason.contains("token budget crítico"));
    }
}
