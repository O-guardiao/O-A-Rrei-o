//! Testes de aceitação derivados de contratos para Recovery Blocks.
//!
//! Pré-condições → input tests, pós-condições → output tests, invariantes → integrity tests.

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Predicado que pode ser avaliado sobre uma string de resultado.
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
}

/// Conjunto de testes de aceitação organizados por categoria contratual.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptanceTest {
    /// Testes derivados de pré-condições (validam entrada / estado inicial).
    pub input_tests: Vec<Predicate>,
    /// Testes derivados de pós-condições (validam saída / resultado).
    pub output_tests: Vec<Predicate>,
    /// Testes derivados de invariantes (validam integridade durante a execução).
    pub integrity_tests: Vec<Predicate>,
}

impl AcceptanceTest {
    /// Avalia todos os predicados em conjunto com lógica AND.
    /// Retorna `true` apenas se todos os testes passarem.
    pub fn evaluate(&self, result: &str) -> bool {
        self.input_tests
            .iter()
            .chain(self.output_tests.iter())
            .chain(self.integrity_tests.iter())
            .all(|p| p.evaluate(result))
    }
}

impl Predicate {
    /// Avalia um único predicado sobre a string fornecida.
    fn evaluate(&self, result: &str) -> bool {
        match self {
            Predicate::NonEmpty => !result.is_empty(),
            Predicate::Contains(sub) => result.contains(sub),
            Predicate::Regex(pattern) => Regex::new(pattern)
                .map(|re| re.is_match(result))
                .unwrap_or(false),
            Predicate::Eq(expected) => result == expected.as_str(),
            Predicate::Range(min, max) => result
                .parse::<i64>()
                .map(|v| v >= *min && v <= *max)
                .unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predicate_non_empty_pass() {
        assert!(Predicate::NonEmpty.evaluate("hello"));
    }

    #[test]
    fn test_predicate_non_empty_fail() {
        assert!(!Predicate::NonEmpty.evaluate(""));
    }

    #[test]
    fn test_predicate_contains_pass() {
        assert!(Predicate::Contains("world".to_string()).evaluate("hello world"));
    }

    #[test]
    fn test_predicate_contains_fail() {
        assert!(!Predicate::Contains("xyz".to_string()).evaluate("hello world"));
    }

    #[test]
    fn test_predicate_regex_pass() {
        assert!(Predicate::Regex(r"^\d+$".to_string()).evaluate("123"));
    }

    #[test]
    fn test_predicate_regex_fail() {
        assert!(!Predicate::Regex(r"^\d+$".to_string()).evaluate("abc"));
    }

    #[test]
    fn test_predicate_eq_pass() {
        assert!(Predicate::Eq("exact".to_string()).evaluate("exact"));
    }

    #[test]
    fn test_predicate_eq_fail() {
        assert!(!Predicate::Eq("exact".to_string()).evaluate("not exact"));
    }

    #[test]
    fn test_predicate_range_pass() {
        assert!(Predicate::Range(10, 20).evaluate("15"));
    }

    #[test]
    fn test_predicate_range_fail() {
        assert!(!Predicate::Range(10, 20).evaluate("25"));
    }

    #[test]
    fn test_predicate_range_invalid_number() {
        assert!(!Predicate::Range(10, 20).evaluate("abc"));
    }

    #[test]
    fn test_acceptance_test_multiple_predicates_and_logic() {
        let test = AcceptanceTest {
            input_tests: vec![Predicate::NonEmpty],
            output_tests: vec![Predicate::Contains("success".to_string())],
            integrity_tests: vec![Predicate::Regex(r"status: ok".to_string())],
        };
        assert!(test.evaluate("success with status: ok"));
        assert!(!test.evaluate("success")); // falha integrity
        assert!(!test.evaluate("status: ok")); // falha output
        assert!(!test.evaluate("")); // falha input
    }

    #[test]
    fn test_acceptance_test_empty_passes() {
        let test = AcceptanceTest {
            input_tests: vec![],
            output_tests: vec![],
            integrity_tests: vec![],
        };
        assert!(test.evaluate("anything"));
    }
}
