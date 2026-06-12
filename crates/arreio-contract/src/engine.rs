use crate::contract::{
    AcceptanceTestCase, Contract, ContractResult, ContractVerificationResult, ContractViolation,
    EvaluationContext, Predicate, PredicateEvaluator, TestType, ViolationType,
};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Motor de verificação de contratos (Design by Contract).
pub struct ContractEngine {
    contracts: HashMap<String, Contract>,
    violations: Vec<ContractViolation>,
}

impl Default for ContractEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ContractEngine {
    pub fn new() -> Self {
        Self {
            contracts: HashMap::new(),
            violations: Vec::new(),
        }
    }

    /// Registra um contrato no motor.
    pub fn register_contract(&mut self, contract: Contract) {
        let name = contract.name.clone();
        self.contracts.insert(name, contract);
    }

    /// Avalia as pré-condições de um contrato.
    pub fn check_preconditions(
        &self,
        contract_name: &str,
        ctx: &EvaluationContext,
    ) -> ContractResult {
        let contract = match self.contracts.get(contract_name) {
            Some(c) => c,
            None => {
                return ContractResult::Unknown {
                    reason: format!("Contrato '{}' não encontrado", contract_name),
                }
            }
        };

        self.evaluate_predicates(
            &contract.preconditions,
            ctx,
            ViolationType::Precondition,
            contract_name,
        )
    }

    /// Avalia as pós-condições de um contrato.
    pub fn check_postconditions(
        &self,
        contract_name: &str,
        ctx: &EvaluationContext,
    ) -> ContractResult {
        let contract = match self.contracts.get(contract_name) {
            Some(c) => c,
            None => {
                return ContractResult::Unknown {
                    reason: format!("Contrato '{}' não encontrado", contract_name),
                }
            }
        };

        self.evaluate_predicates(
            &contract.postconditions,
            ctx,
            ViolationType::Postcondition,
            contract_name,
        )
    }

    /// Avalia os invariantes de um contrato.
    pub fn check_invariants(&self, contract_name: &str, ctx: &EvaluationContext) -> ContractResult {
        let contract = match self.contracts.get(contract_name) {
            Some(c) => c,
            None => {
                return ContractResult::Unknown {
                    reason: format!("Contrato '{}' não encontrado", contract_name),
                }
            }
        };

        self.evaluate_predicates(
            &contract.invariants,
            ctx,
            ViolationType::Invariant,
            contract_name,
        )
    }

    /// Verificação completa: pré → exec → pós → invariante.
    pub fn verify_contract<F>(
        &mut self,
        contract_name: &str,
        ctx: &EvaluationContext,
        exec: F,
    ) -> ContractVerificationResult
    where
        F: FnOnce() -> anyhow::Result<EvaluationContext>,
    {
        let pre = self.check_preconditions(contract_name, ctx);

        let exec_result = if pre == ContractResult::Satisfied {
            exec()
        } else {
            Err(anyhow::anyhow!("Pré-condições não satisfeitas"))
        };

        let post = if let Ok(ref post_ctx) = exec_result {
            self.check_postconditions(contract_name, post_ctx)
        } else {
            ContractResult::Unknown {
                reason: "Execução falhou, pós-condições não avaliadas".to_string(),
            }
        };

        let inv = if let Ok(ref post_ctx) = exec_result {
            self.check_invariants(contract_name, post_ctx)
        } else {
            ContractResult::Unknown {
                reason: "Execução falhou, invariantes não avaliados".to_string(),
            }
        };

        let overall = match (&pre, &exec_result, &post, &inv) {
            (
                ContractResult::Satisfied,
                Ok(_),
                ContractResult::Satisfied,
                ContractResult::Satisfied,
            ) => ContractResult::Satisfied,
            _ => ContractResult::Violated {
                predicate_id: "overall".to_string(),
                reason: "Falha na verificação completa do contrato".to_string(),
            },
        };

        ContractVerificationResult {
            preconditions: pre,
            execution_result: exec_result,
            postconditions: post,
            invariants: inv,
            overall,
        }
    }

    /// Deriva casos de teste de aceitação a partir de um contrato.
    pub fn derive_acceptance_tests(&self, contract_name: &str) -> Vec<AcceptanceTestCase> {
        let contract = match self.contracts.get(contract_name) {
            Some(c) => c,
            None => return Vec::new(),
        };

        let mut tests = Vec::new();

        for pre in &contract.preconditions {
            tests.push(AcceptanceTestCase {
                name: format!("test_input_{}", pre.id),
                test_type: TestType::InputTest,
                predicate: Predicate {
                    id: pre.id.clone(),
                    description: pre.description.clone(),
                    expression: pre.expression.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Teste de entrada para: {}", pre.description),
                    },
                },
                expected_result: true,
            });
        }

        for post in &contract.postconditions {
            tests.push(AcceptanceTestCase {
                name: format!("test_output_{}", post.id),
                test_type: TestType::OutputTest,
                predicate: Predicate {
                    id: post.id.clone(),
                    description: post.description.clone(),
                    expression: post.expression.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Teste de saída para: {}", post.description),
                    },
                },
                expected_result: true,
            });
        }

        for inv in &contract.invariants {
            tests.push(AcceptanceTestCase {
                name: format!("test_integrity_{}", inv.id),
                test_type: TestType::IntegrityTest,
                predicate: Predicate {
                    id: inv.id.clone(),
                    description: inv.description.clone(),
                    expression: inv.expression.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Teste de integridade para: {}", inv.description),
                    },
                },
                expected_result: true,
            });
        }

        tests
    }

    /// Retorna todas as violações registradas.
    pub fn violations(&self) -> &[ContractViolation] {
        &self.violations
    }

    /// Limpa o histórico de violações.
    pub fn clear_violations(&mut self) {
        self.violations.clear();
    }

    /// Método interno para avaliar uma lista de predicados.
    fn evaluate_predicates(
        &self,
        predicates: &[Predicate],
        ctx: &EvaluationContext,
        _vtype: ViolationType,
        _contract_name: &str,
    ) -> ContractResult {
        for pred in predicates {
            let satisfied = match &pred.evaluator {
                PredicateEvaluator::RuntimeAssert(f) => f(ctx),
                PredicateEvaluator::StaticCheck { .. } => {
                    // Verificações estáticas não são avaliadas em runtime.
                    continue;
                }
                PredicateEvaluator::LlmEvaluated { .. } => {
                    // LLM avaliado é tratado como desconhecido em runtime puro.
                    continue;
                }
            };

            if !satisfied {
                return ContractResult::Violated {
                    predicate_id: pred.id.clone(),
                    reason: format!("Predicado '{}' falhou: {}", pred.id, pred.description),
                };
            }
        }

        ContractResult::Satisfied
    }

    /// Registra uma violação no histórico.
    #[allow(dead_code)]
    fn record_violation(
        &mut self,
        contract_name: &str,
        predicate_id: &str,
        vtype: ViolationType,
        ctx: &EvaluationContext,
    ) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.violations.push(ContractViolation {
            contract_name: contract_name.to_string(),
            predicate_id: predicate_id.to_string(),
            violation_type: vtype,
            timestamp,
            context: ctx.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Predicate, PredicateEvaluator};

    fn make_ctx_with_input(key: &str, value: serde_json::Value) -> EvaluationContext {
        let mut ctx = EvaluationContext::default();
        ctx.inputs.insert(key.to_string(), value);
        ctx
    }

    #[test]
    fn register_and_retrieve_contract() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "test_contract".to_string(),
            preconditions: vec![Predicate {
                id: "p1".to_string(),
                description: "x deve ser positivo".to_string(),
                expression: "x > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.inputs
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .map(|v| v > 0)
                        .unwrap_or(false)
                })),
            }],
            postconditions: vec![],
            invariants: vec![],
        };

        engine.register_contract(contract);
        let ctx = make_ctx_with_input("x", serde_json::json!(5));
        let result = engine.check_preconditions("test_contract", &ctx);
        assert_eq!(result, ContractResult::Satisfied);
    }

    #[test]
    fn precondition_passes() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "pre_pass".to_string(),
            preconditions: vec![Predicate {
                id: "p1".to_string(),
                description: "x > 0".to_string(),
                expression: "x > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.inputs
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .map(|v| v > 0)
                        .unwrap_or(false)
                })),
            }],
            postconditions: vec![],
            invariants: vec![],
        };
        engine.register_contract(contract);

        let ctx = make_ctx_with_input("x", serde_json::json!(10));
        assert_eq!(
            engine.check_preconditions("pre_pass", &ctx),
            ContractResult::Satisfied
        );
    }

    #[test]
    fn precondition_fails() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "pre_fail".to_string(),
            preconditions: vec![Predicate {
                id: "p1".to_string(),
                description: "x > 0".to_string(),
                expression: "x > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.inputs
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .map(|v| v > 0)
                        .unwrap_or(false)
                })),
            }],
            postconditions: vec![],
            invariants: vec![],
        };
        engine.register_contract(contract);

        let ctx = make_ctx_with_input("x", serde_json::json!(-5));
        let result = engine.check_preconditions("pre_fail", &ctx);
        assert!(
            matches!(result, ContractResult::Violated { ref predicate_id, .. } if predicate_id == "p1"),
            "Esperado Violated com predicate_id 'p1', obtido {:?}",
            result
        );
    }

    #[test]
    fn postcondition_check() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "post_check".to_string(),
            preconditions: vec![],
            postconditions: vec![Predicate {
                id: "p1".to_string(),
                description: "resultado deve ser positivo".to_string(),
                expression: "result > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.outputs
                        .get("result")
                        .and_then(|v| v.as_i64())
                        .map(|v| v > 0)
                        .unwrap_or(false)
                })),
            }],
            invariants: vec![],
        };
        engine.register_contract(contract);

        let mut ctx = EvaluationContext::default();
        ctx.outputs
            .insert("result".to_string(), serde_json::json!(42));
        assert_eq!(
            engine.check_postconditions("post_check", &ctx),
            ContractResult::Satisfied
        );

        ctx.outputs
            .insert("result".to_string(), serde_json::json!(-1));
        let result = engine.check_postconditions("post_check", &ctx);
        assert!(matches!(result, ContractResult::Violated { .. }));
    }

    #[test]
    fn invariant_before_and_after() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "inv_check".to_string(),
            preconditions: vec![],
            postconditions: vec![],
            invariants: vec![Predicate {
                id: "i1".to_string(),
                description: "contador não pode ser negativo".to_string(),
                expression: "counter >= 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.state
                        .get("counter")
                        .and_then(|v| v.as_i64())
                        .map(|v| v >= 0)
                        .unwrap_or(true)
                })),
            }],
        };
        engine.register_contract(contract);

        let mut ctx = EvaluationContext::default();
        ctx.state
            .insert("counter".to_string(), serde_json::json!(5));
        assert_eq!(
            engine.check_invariants("inv_check", &ctx),
            ContractResult::Satisfied
        );

        ctx.state
            .insert("counter".to_string(), serde_json::json!(-3));
        let result = engine.check_invariants("inv_check", &ctx);
        assert!(matches!(result, ContractResult::Violated { .. }));
    }

    #[test]
    fn full_contract_verification() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "full".to_string(),
            preconditions: vec![Predicate {
                id: "pre1".to_string(),
                description: "x > 0".to_string(),
                expression: "x > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.inputs
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .map(|v| v > 0)
                        .unwrap_or(false)
                })),
            }],
            postconditions: vec![Predicate {
                id: "post1".to_string(),
                description: "resultado = x * 2".to_string(),
                expression: "result == x * 2".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    let x = ctx.inputs.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
                    let result = ctx
                        .outputs
                        .get("result")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    result == x * 2
                })),
            }],
            invariants: vec![],
        };
        engine.register_contract(contract);

        let mut ctx = EvaluationContext::default();
        ctx.inputs.insert("x".to_string(), serde_json::json!(7));

        let result = engine.verify_contract("full", &ctx, || {
            let mut out = EvaluationContext::default();
            out.inputs.insert("x".to_string(), serde_json::json!(7));
            out.outputs
                .insert("result".to_string(), serde_json::json!(14));
            Ok(out)
        });

        assert_eq!(result.preconditions, ContractResult::Satisfied);
        assert!(result.execution_result.is_ok());
        assert_eq!(result.postconditions, ContractResult::Satisfied);
        assert_eq!(result.overall, ContractResult::Satisfied);
    }

    #[test]
    fn derive_acceptance_tests_from_contract() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "tests".to_string(),
            preconditions: vec![Predicate {
                id: "pre1".to_string(),
                description: "x > 0".to_string(),
                expression: "x > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|_| true)),
            }],
            postconditions: vec![Predicate {
                id: "post1".to_string(),
                description: "result > 0".to_string(),
                expression: "result > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|_| true)),
            }],
            invariants: vec![Predicate {
                id: "inv1".to_string(),
                description: "state >= 0".to_string(),
                expression: "state >= 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|_| true)),
            }],
        };
        engine.register_contract(contract);

        let tests = engine.derive_acceptance_tests("tests");
        assert_eq!(tests.len(), 3);

        assert!(tests.iter().any(|t| t.test_type == TestType::InputTest));
        assert!(tests.iter().any(|t| t.test_type == TestType::OutputTest));
        assert!(tests.iter().any(|t| t.test_type == TestType::IntegrityTest));
    }

    #[test]
    fn nl2contract_parses_simple_spec() {
        use crate::nl2contract::NL2Contract;

        let spec = r#"
name: SomaSegura
precondition: x deve ser um número
precondition: y deve ser um número
postcondition: resultado deve ser x + y
invariant: resultado não pode transbordar
"#;

        let contract = NL2Contract::parse(spec).unwrap();
        assert_eq!(contract.name, "SomaSegura");
        assert_eq!(contract.preconditions.len(), 2);
        assert_eq!(contract.postconditions.len(), 1);
        assert_eq!(contract.invariants.len(), 1);
    }

    #[test]
    fn builtin_predicates_non_empty() {
        use crate::predicates;

        assert!(predicates::non_empty(&serde_json::json!("hello")));
        assert!(!predicates::non_empty(&serde_json::json!("")));
        assert!(predicates::non_empty(&serde_json::json!([1, 2, 3])));
        assert!(!predicates::non_empty(&serde_json::json!([])));
        assert!(predicates::non_empty(&serde_json::json!({"a": 1})));
        assert!(!predicates::non_empty(&serde_json::json!({})));
        assert!(!predicates::non_empty(&serde_json::Value::Null));
    }

    #[test]
    fn violation_tracking() {
        let mut engine = ContractEngine::new();
        let contract = Contract {
            name: "viol".to_string(),
            preconditions: vec![Predicate {
                id: "pre1".to_string(),
                description: "x > 0".to_string(),
                expression: "x > 0".to_string(),
                evaluator: PredicateEvaluator::RuntimeAssert(Box::new(|ctx| {
                    ctx.inputs
                        .get("x")
                        .and_then(|v| v.as_i64())
                        .map(|v| v > 0)
                        .unwrap_or(false)
                })),
            }],
            postconditions: vec![],
            invariants: vec![],
        };
        engine.register_contract(contract);

        assert!(engine.violations().is_empty());

        // Força uma violação registrando manualmente
        let ctx = make_ctx_with_input("x", serde_json::json!(-1));
        engine.record_violation("viol", "pre1", ViolationType::Precondition, &ctx);

        assert_eq!(engine.violations().len(), 1);
        assert_eq!(engine.violations()[0].contract_name, "viol");
        assert_eq!(engine.violations()[0].predicate_id, "pre1");
        assert_eq!(
            engine.violations()[0].violation_type,
            ViolationType::Precondition
        );

        engine.clear_violations();
        assert!(engine.violations().is_empty());
    }
}
