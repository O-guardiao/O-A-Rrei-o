use crate::specification::SpecificationStatement;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Predicado de contrato para Design by Contract.
///
/// Pode ser avaliado diretamente sobre strings de entrada/saída/estado.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Predicate {
    /// A string não deve ser vazia.
    NonEmpty,
    /// A string deve conter o subtexto especificado.
    Contains(String),
    /// A string deve corresponder à expressão regular.
    Regex(String),
    /// A string deve ser exatamente igual.
    Eq(String),
    /// A string, interpretada como número, deve estar no intervalo fechado [min, max].
    Range(i64, i64),
    /// Conjunção lógica de dois predicados.
    And(Box<Predicate>, Box<Predicate>),
    /// Disjunção lógica de dois predicados.
    Or(Box<Predicate>, Box<Predicate>),
}

impl Predicate {
    /// Avalia o predicado sobre uma string.
    pub fn verify(&self, input: &str) -> bool {
        match self {
            Predicate::NonEmpty => !input.is_empty(),
            Predicate::Contains(sub) => input.contains(sub),
            Predicate::Regex(pattern) => Regex::new(pattern)
                .map(|re| re.is_match(input))
                .unwrap_or(false),
            Predicate::Eq(expected) => input == expected.as_str(),
            Predicate::Range(min, max) => input
                .parse::<i64>()
                .map(|v| v >= *min && v <= *max)
                .unwrap_or(false),
            Predicate::And(left, right) => left.verify(input) && right.verify(input),
            Predicate::Or(left, right) => left.verify(input) || right.verify(input),
        }
    }
}

/// Contrato com precondições, pós-condições e invariantes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contract {
    pub preconditions: Vec<Predicate>,
    pub postconditions: Vec<Predicate>,
    pub invariants: Vec<Predicate>,
}

impl Contract {
    /// Cria um novo contrato.
    pub fn new(
        preconditions: Vec<Predicate>,
        postconditions: Vec<Predicate>,
        invariants: Vec<Predicate>,
    ) -> Self {
        Self {
            preconditions,
            postconditions,
            invariants,
        }
    }

    /// Verifica se todas as pré-condições são satisfeitas pelo input.
    pub fn verify_input(&self, input: &str) -> bool {
        self.preconditions.iter().all(|p| p.verify(input))
    }

    /// Verifica se todas as pós-condições são satisfeitas pelo output.
    pub fn verify_output(&self, output: &str) -> bool {
        self.postconditions.iter().all(|p| p.verify(output))
    }

    /// Verifica se todos os invariantes são mantidos pelo estado.
    pub fn verify_integrity(&self, state: &str) -> bool {
        self.invariants.iter().all(|p| p.verify(state))
    }

    /// Deriva um contrato a partir de uma especificação formal.
    ///
    /// - Pré-condições ← `pre` da especificação (como `Contains`).
    /// - Pós-condições ← `post` da especificação (como `Contains`).
    /// - Invariantes ← `frame` da especificação (como `Contains`).
    pub fn from_specification(spec: &SpecificationStatement) -> Self {
        Self {
            preconditions: vec![Predicate::Contains(spec.pre.clone())],
            postconditions: vec![Predicate::Contains(spec.post.clone())],
            invariants: vec![Predicate::Contains(spec.frame.clone())],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_verifies_precondition() {
        let contract = Contract::new(
            vec![
                Predicate::NonEmpty,
                Predicate::Contains("hello".to_string()),
            ],
            vec![],
            vec![],
        );
        assert!(contract.verify_input("hello world"));
        assert!(!contract.verify_input(""));
        assert!(!contract.verify_input("goodbye"));
    }

    #[test]
    fn contract_verifies_postcondition() {
        let contract = Contract::new(vec![], vec![Predicate::Eq("done".to_string())], vec![]);
        assert!(contract.verify_output("done"));
        assert!(!contract.verify_output("done!"));
    }

    #[test]
    fn contract_verifies_invariant() {
        let contract = Contract::new(vec![], vec![], vec![Predicate::Regex(r"^\d+$".to_string())]);
        assert!(contract.verify_integrity("12345"));
        assert!(!contract.verify_integrity("abc"));
    }

    #[test]
    fn predicate_and_logic() {
        let pred = Predicate::And(
            Box::new(Predicate::NonEmpty),
            Box::new(Predicate::Contains("x".to_string())),
        );
        assert!(pred.verify("xyz"));
        assert!(!pred.verify(""));
        assert!(!pred.verify("abc"));
    }

    #[test]
    fn predicate_or_logic() {
        let pred = Predicate::Or(
            Box::new(Predicate::Eq("a".to_string())),
            Box::new(Predicate::Eq("b".to_string())),
        );
        assert!(pred.verify("a"));
        assert!(pred.verify("b"));
        assert!(!pred.verify("c"));
    }

    #[test]
    fn contract_from_specification_derives_correctly() {
        let spec = SpecificationStatement::new("x > 0", "result > 0", "counter >= 0");
        let contract = Contract::from_specification(&spec);

        assert_eq!(contract.preconditions.len(), 1);
        assert_eq!(contract.postconditions.len(), 1);
        assert_eq!(contract.invariants.len(), 1);

        assert!(contract.verify_input("x > 0"));
        assert!(!contract.verify_input("x < 0"));

        assert!(contract.verify_output("result > 0"));
        assert!(!contract.verify_output("result < 0"));

        assert!(contract.verify_integrity("counter >= 0"));
        assert!(!contract.verify_integrity("counter < 0"));
    }

    #[test]
    fn predicate_range_verification() {
        let pred = Predicate::Range(10, 20);
        assert!(pred.verify("15"));
        assert!(!pred.verify("25"));
        assert!(!pred.verify("abc"));
    }

    #[test]
    fn predicate_regex_verification() {
        let pred = Predicate::Regex(r"^\d+$".to_string());
        assert!(pred.verify("123"));
        assert!(!pred.verify("abc"));
    }
}
