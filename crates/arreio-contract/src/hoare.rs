use crate::dbc::Predicate;
use anyhow::{anyhow, Result};

/// Tripla de Hoare: {precondition} statement {postcondition}.
#[derive(Debug, Clone, PartialEq)]
pub struct HoareTriple {
    pub precondition: Predicate,
    pub statement: String,
    pub postcondition: Predicate,
}

impl HoareTriple {
    /// Cria uma nova tripla de Hoare.
    pub fn new(
        precondition: Predicate,
        statement: impl Into<String>,
        postcondition: Predicate,
    ) -> Self {
        Self {
            precondition,
            statement: statement.into(),
            postcondition,
        }
    }
}

/// Motor de regras de Hoare Logic.
pub struct HoareLogic;

impl HoareLogic {
    /// Regra de atribuição: calcula a weakest precondition para `var := expr` dado `post`.
    ///
    /// Aproximação textual: substitui ocorrências de `var` por `expr` nas strings
    /// dos predicados `Eq`, `Contains` e `Regex`.
    pub fn assignment_rule(var: &str, expr: &str, post: &Predicate) -> Predicate {
        Self::substitute_predicate(post, var, expr)
    }

    /// Regra de sequência: dados {P} S1 {Q} e {Q} S2 {R}, deriva {P} S1;S2 {R}.
    pub fn sequence_rule(first: &HoareTriple, second: &HoareTriple) -> Result<HoareTriple> {
        if first.postcondition != second.precondition {
            return Err(anyhow!(
                "Postcondição do primeiro ({:?}) não coincide com pré-condição do segundo ({:?})",
                first.postcondition,
                second.precondition
            ));
        }

        Ok(HoareTriple {
            precondition: first.precondition.clone(),
            statement: format!("{}; {}", first.statement, second.statement),
            postcondition: second.postcondition.clone(),
        })
    }

    /// Regra condicional: dados `condition`, {P∧B} S1 {Q} e {P∧¬B} S2 {Q},
    /// deriva {P} if B then S1 else S2 {Q}.
    ///
    /// Nesta implementação simplificada, a pré-condição resultante é a conjunção
    /// das pré-condições dos dois ramos (aproximação de P∧B e P∧¬B).
    pub fn conditional_rule(
        condition: &str,
        then_triple: &HoareTriple,
        else_triple: &HoareTriple,
    ) -> Result<HoareTriple> {
        if then_triple.postcondition != else_triple.postcondition {
            return Err(anyhow!(
                "Postcondições dos ramos then ({:?}) e else ({:?}) divergem",
                then_triple.postcondition,
                else_triple.postcondition
            ));
        }

        Ok(HoareTriple {
            precondition: Predicate::And(
                Box::new(then_triple.precondition.clone()),
                Box::new(else_triple.precondition.clone()),
            ),
            statement: format!(
                "if {} then {} else {}",
                condition, then_triple.statement, else_triple.statement
            ),
            postcondition: then_triple.postcondition.clone(),
        })
    }

    fn substitute_predicate(pred: &Predicate, var: &str, expr: &str) -> Predicate {
        match pred {
            Predicate::Eq(s) => Predicate::Eq(s.replace(var, expr)),
            Predicate::Contains(s) => Predicate::Contains(s.replace(var, expr)),
            Predicate::Regex(s) => Predicate::Regex(s.replace(var, expr)),
            Predicate::And(l, r) => Predicate::And(
                Box::new(Self::substitute_predicate(l, var, expr)),
                Box::new(Self::substitute_predicate(r, var, expr)),
            ),
            Predicate::Or(l, r) => Predicate::Or(
                Box::new(Self::substitute_predicate(l, var, expr)),
                Box::new(Self::substitute_predicate(r, var, expr)),
            ),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hoare_triple_construction() {
        let triple = HoareTriple::new(
            Predicate::NonEmpty,
            "x := 5",
            Predicate::Eq("x=5".to_string()),
        );
        assert_eq!(triple.statement, "x := 5");
        assert_eq!(triple.precondition, Predicate::NonEmpty);
        assert_eq!(triple.postcondition, Predicate::Eq("x=5".to_string()));
    }

    #[test]
    fn assignment_rule_generates_weakest_precondition() {
        let post = Predicate::Eq("x = 10".to_string());
        let wp = HoareLogic::assignment_rule("x", "y + 1", &post);
        assert_eq!(wp, Predicate::Eq("y + 1 = 10".to_string()));
    }

    #[test]
    fn assignment_rule_with_contains() {
        let post = Predicate::Contains("result: x".to_string());
        let wp = HoareLogic::assignment_rule("x", "42", &post);
        assert_eq!(wp, Predicate::Contains("result: 42".to_string()));
    }

    #[test]
    fn sequence_rule_combines_two_triples() {
        let first = HoareTriple::new(
            Predicate::NonEmpty,
            "x := 1",
            Predicate::Eq("x=1".to_string()),
        );
        let second = HoareTriple::new(
            Predicate::Eq("x=1".to_string()),
            "x := x + 1",
            Predicate::Eq("x=2".to_string()),
        );

        let combined = HoareLogic::sequence_rule(&first, &second).unwrap();
        assert_eq!(combined.precondition, Predicate::NonEmpty);
        assert_eq!(combined.postcondition, Predicate::Eq("x=2".to_string()));
        assert!(combined.statement.contains("x := 1"));
        assert!(combined.statement.contains("x := x + 1"));
    }

    #[test]
    fn sequence_rule_fails_on_mismatch() {
        let first = HoareTriple::new(Predicate::NonEmpty, "S1", Predicate::Eq("a".to_string()));
        let second = HoareTriple::new(
            Predicate::Eq("b".to_string()),
            "S2",
            Predicate::Eq("c".to_string()),
        );

        assert!(HoareLogic::sequence_rule(&first, &second).is_err());
    }

    #[test]
    fn conditional_rule_combines_branches() {
        let then_branch = HoareTriple::new(
            Predicate::NonEmpty,
            "x := 1",
            Predicate::Eq("done".to_string()),
        );
        let else_branch = HoareTriple::new(
            Predicate::Contains("fallback".to_string()),
            "x := 0",
            Predicate::Eq("done".to_string()),
        );

        let result = HoareLogic::conditional_rule("x > 0", &then_branch, &else_branch).unwrap();
        assert_eq!(
            result.precondition,
            Predicate::And(
                Box::new(Predicate::NonEmpty),
                Box::new(Predicate::Contains("fallback".to_string()))
            )
        );
        assert_eq!(result.postcondition, Predicate::Eq("done".to_string()));
        assert!(result.statement.contains("if x > 0"));
    }

    #[test]
    fn conditional_rule_fails_on_divergent_posts() {
        let then_branch =
            HoareTriple::new(Predicate::NonEmpty, "S1", Predicate::Eq("a".to_string()));
        let else_branch =
            HoareTriple::new(Predicate::NonEmpty, "S2", Predicate::Eq("b".to_string()));

        assert!(HoareLogic::conditional_rule("true", &then_branch, &else_branch).is_err());
    }
}
