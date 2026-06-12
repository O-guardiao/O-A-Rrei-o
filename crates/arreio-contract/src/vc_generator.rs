use crate::dbc::{Contract, Predicate};
use anyhow::Result;
use regex::Regex;

/// Condição de verificação gerada pelo VC Generator.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationCondition {
    pub description: String,
    pub obligation: Predicate,
    pub proven: bool,
}

/// Gerador de Condições de Verificação (VC Generator).
///
/// Gera obrigações de prova a partir de um contrato e de um trecho de código.
/// A verificação é heurística (string matching), sem SMT solver real.
pub struct VcGenerator;

impl VcGenerator {
    /// Gera condições de verificação a partir de um contrato e código fonte.
    pub fn generate(contract: &Contract, code: &str) -> Result<Vec<VerificationCondition>> {
        let mut vcs = Vec::new();

        for (i, pre) in contract.preconditions.iter().enumerate() {
            let proven = Self::heuristic_check(pre, code);
            vcs.push(VerificationCondition {
                description: format!("Pré-condição {} satisfeita na entrada", i + 1),
                obligation: pre.clone(),
                proven,
            });
        }

        for (i, post) in contract.postconditions.iter().enumerate() {
            let proven = Self::heuristic_check(post, code);
            vcs.push(VerificationCondition {
                description: format!("Pós-condição {} garantida pelo código", i + 1),
                obligation: post.clone(),
                proven,
            });
        }

        for (i, inv) in contract.invariants.iter().enumerate() {
            let proven = Self::heuristic_check(inv, code);
            vcs.push(VerificationCondition {
                description: format!("Invariante {} preservado", i + 1),
                obligation: inv.clone(),
                proven,
            });
        }

        Ok(vcs)
    }

    /// Heurística de string matching para decidir se uma obrigação é "proven".
    fn heuristic_check(pred: &Predicate, code: &str) -> bool {
        match pred {
            Predicate::NonEmpty => !code.is_empty(),
            Predicate::Contains(sub) => code.contains(sub),
            Predicate::Regex(pattern) => Regex::new(pattern)
                .map(|re| re.is_match(code))
                .unwrap_or(false),
            Predicate::Eq(expected) => code == expected.as_str(),
            Predicate::Range(min, max) => code
                .parse::<i64>()
                .map(|v| v >= *min && v <= *max)
                .unwrap_or(false),
            Predicate::And(left, right) => {
                Self::heuristic_check(left, code) && Self::heuristic_check(right, code)
            }
            Predicate::Or(left, right) => {
                Self::heuristic_check(left, code) || Self::heuristic_check(right, code)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vc_generator_creates_conditions_for_simple_code() {
        let contract = Contract::new(
            vec![Predicate::NonEmpty],
            vec![Predicate::Contains("return".to_string())],
            vec![Predicate::Regex(r"^fn ".to_string())],
        );
        let code = "fn main() { return 42; }";

        let vcs = VcGenerator::generate(&contract, code).unwrap();
        assert_eq!(vcs.len(), 3);

        assert_eq!(vcs[0].description, "Pré-condição 1 satisfeita na entrada");
        assert!(vcs[0].proven);

        assert_eq!(vcs[1].description, "Pós-condição 1 garantida pelo código");
        assert!(vcs[1].proven);

        assert_eq!(vcs[2].description, "Invariante 1 preservado");
        assert!(vcs[2].proven);
    }

    #[test]
    fn vc_generator_marks_unproven_when_obligation_not_met() {
        let contract = Contract::new(
            vec![Predicate::Contains("missing".to_string())],
            vec![],
            vec![],
        );
        let code = "fn main() {}";

        let vcs = VcGenerator::generate(&contract, code).unwrap();
        assert_eq!(vcs.len(), 1);
        assert!(!vcs[0].proven);
    }

    #[test]
    fn vc_generator_marks_proven_when_obligation_is_trivial() {
        let contract = Contract::new(vec![Predicate::NonEmpty], vec![], vec![]);
        let code = " "; // não vazio

        let vcs = VcGenerator::generate(&contract, code).unwrap();
        assert_eq!(vcs.len(), 1);
        assert!(vcs[0].proven);
        assert_eq!(vcs[0].obligation, Predicate::NonEmpty);
    }

    #[test]
    fn vc_generator_handles_and_predicate() {
        let contract = Contract::new(
            vec![Predicate::And(
                Box::new(Predicate::NonEmpty),
                Box::new(Predicate::Contains("foo".to_string())),
            )],
            vec![],
            vec![],
        );
        let code = "foo bar";

        let vcs = VcGenerator::generate(&contract, code).unwrap();
        assert_eq!(vcs.len(), 1);
        assert!(vcs[0].proven);
    }

    #[test]
    fn vc_generator_handles_or_predicate() {
        let contract = Contract::new(
            vec![Predicate::Or(
                Box::new(Predicate::Contains("foo".to_string())),
                Box::new(Predicate::Contains("bar".to_string())),
            )],
            vec![],
            vec![],
        );
        let code = "baz";

        let vcs = VcGenerator::generate(&contract, code).unwrap();
        assert_eq!(vcs.len(), 1);
        assert!(!vcs[0].proven);
    }
}
