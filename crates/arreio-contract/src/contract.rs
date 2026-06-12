use serde_json;
use std::collections::HashMap;

/// Contexto de avaliação para predicados de contrato.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvaluationContext {
    pub inputs: HashMap<String, serde_json::Value>,
    pub outputs: HashMap<String, serde_json::Value>,
    pub state: HashMap<String, serde_json::Value>,
    pub metadata: HashMap<String, String>,
}

/// Avaliador de predicado: como a condição será verificada.
pub enum PredicateEvaluator {
    /// Asserção em tempo de execução.
    RuntimeAssert(Box<dyn Fn(&EvaluationContext) -> bool + Send + Sync>),
    /// Verificação estática (análise de código).
    StaticCheck { checker: String },
    /// Avaliação por LLM (condição em linguagem natural).
    LlmEvaluated { prompt: String },
}

impl std::fmt::Debug for PredicateEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PredicateEvaluator::RuntimeAssert(_) => f.debug_struct("RuntimeAssert").finish(),
            PredicateEvaluator::StaticCheck { checker } => f
                .debug_struct("StaticCheck")
                .field("checker", checker)
                .finish(),
            PredicateEvaluator::LlmEvaluated { prompt } => f
                .debug_struct("LlmEvaluated")
                .field("prompt", prompt)
                .finish(),
        }
    }
}

/// Predicado de contrato: uma condição que deve ser satisfeita.
#[derive(Debug)]
pub struct Predicate {
    pub id: String,
    pub description: String,
    pub expression: String,
    pub evaluator: PredicateEvaluator,
}

/// Contrato com precondições, pós-condições e invariantes.
#[derive(Debug)]
pub struct Contract {
    pub name: String,
    pub preconditions: Vec<Predicate>,
    pub postconditions: Vec<Predicate>,
    pub invariants: Vec<Predicate>,
}

/// Resultado da avaliação de uma condição de contrato.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractResult {
    Satisfied,
    Violated {
        predicate_id: String,
        reason: String,
    },
    Unknown {
        reason: String,
    },
}

/// Tipo de violação de contrato.
#[derive(Debug, Clone, PartialEq)]
pub enum ViolationType {
    Precondition,
    Postcondition,
    Invariant,
}

/// Registro de uma violação de contrato.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractViolation {
    pub contract_name: String,
    pub predicate_id: String,
    pub violation_type: ViolationType,
    pub timestamp: u64,
    pub context: EvaluationContext,
}

/// Resultado completo da verificação de contrato.
#[derive(Debug)]
pub struct ContractVerificationResult {
    pub preconditions: ContractResult,
    pub execution_result: anyhow::Result<EvaluationContext>,
    pub postconditions: ContractResult,
    pub invariants: ContractResult,
    pub overall: ContractResult,
}

/// Tipo de caso de teste de aceitação.
#[derive(Debug, Clone, PartialEq)]
pub enum TestType {
    InputTest,
    OutputTest,
    IntegrityTest,
}

/// Caso de teste de aceitação derivado de um contrato.
#[derive(Debug)]
pub struct AcceptanceTestCase {
    pub name: String,
    pub test_type: TestType,
    pub predicate: Predicate,
    pub expected_result: bool,
}
