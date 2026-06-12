//! Regras de reescrita para Equality Saturation.

use crate::egraph::{EClassId, EGraph, ENode};
use std::collections::HashMap;

/// Padrão para pattern matching no e-graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pattern {
    Wildcard(String),
    Const(i64),
    Add(Box<Pattern>, Box<Pattern>),
    Mul(Box<Pattern>, Box<Pattern>),
}

impl Pattern {
    /// Tenta fazer match do padrão em uma e-class, retornando bindings se suceder.
    pub fn match_in_eclass(
        &self,
        egraph: &EGraph,
        id: EClassId,
        bindings: &mut HashMap<String, EClassId>,
    ) -> bool {
        let root = egraph.union_find.parent.get(id).copied().unwrap_or(id);
        match self {
            Pattern::Wildcard(name) => {
                if let Some(&bound) = bindings.get(name) {
                    egraph
                        .union_find
                        .parent
                        .get(bound)
                        .copied()
                        .unwrap_or(bound)
                        == root
                } else {
                    bindings.insert(name.clone(), root);
                    true
                }
            }
            Pattern::Const(c) => {
                if let Some(class) = egraph.classes.get(&root) {
                    class.nodes.iter().any(|n| n.op == format!("Const:{}", c))
                } else {
                    false
                }
            }
            Pattern::Add(p1, p2) => {
                if let Some(class) = egraph.classes.get(&root) {
                    class.nodes.iter().any(|n| {
                        n.op == "Add"
                            && n.children.len() == 2
                            && p1.match_in_eclass(egraph, n.children[0], bindings)
                            && p2.match_in_eclass(egraph, n.children[1], bindings)
                    })
                } else {
                    false
                }
            }
            Pattern::Mul(p1, p2) => {
                if let Some(class) = egraph.classes.get(&root) {
                    class.nodes.iter().any(|n| {
                        n.op == "Mul"
                            && n.children.len() == 2
                            && p1.match_in_eclass(egraph, n.children[0], bindings)
                            && p2.match_in_eclass(egraph, n.children[1], bindings)
                    })
                } else {
                    false
                }
            }
        }
    }

    /// Instancia um padrão com bindings, adicionando ao e-graph.
    pub fn instantiate(
        &self,
        egraph: &mut EGraph,
        bindings: &HashMap<String, EClassId>,
    ) -> EClassId {
        match self {
            Pattern::Wildcard(name) => *bindings.get(name).expect("wildcard não ligado"),
            Pattern::Const(c) => {
                let node = ENode::new(format!("Const:{}", c), vec![]);
                egraph.add_node(node)
            }
            Pattern::Add(p1, p2) => {
                let id1 = p1.instantiate(egraph, bindings);
                let id2 = p2.instantiate(egraph, bindings);
                let node = ENode::new("Add", vec![id1, id2]);
                egraph.add_node(node)
            }
            Pattern::Mul(p1, p2) => {
                let id1 = p1.instantiate(egraph, bindings);
                let id2 = p2.instantiate(egraph, bindings);
                let node = ENode::new("Mul", vec![id1, id2]);
                egraph.add_node(node)
            }
        }
    }
}

/// Regra de reescrita: lhs -> rhs.
#[derive(Debug, Clone)]
pub struct RewriteRule {
    pub name: String,
    pub lhs: Pattern,
    pub rhs: Pattern,
}

impl RewriteRule {
    pub fn new(name: impl Into<String>, lhs: Pattern, rhs: Pattern) -> Self {
        Self {
            name: name.into(),
            lhs,
            rhs,
        }
    }

    /// Aplica a regra em todo o e-graph, retornando pares de merges realizados.
    pub fn apply(&self, egraph: &mut EGraph) -> Vec<(EClassId, EClassId)> {
        let mut merges = Vec::new();
        let class_ids: Vec<EClassId> = egraph.classes.keys().copied().collect();

        for &id in &class_ids {
            let root = egraph.find(id);
            let mut bindings = HashMap::new();
            if self.lhs.match_in_eclass(egraph, root, &mut bindings) {
                let rhs_id = self.rhs.instantiate(egraph, &bindings);
                let lhs_root = egraph.find(root);
                let rhs_root = egraph.find(rhs_id);
                if lhs_root != rhs_root {
                    let merged = egraph.merge(lhs_root, rhs_root);
                    merges.push((lhs_root, merged));
                }
            }
        }
        merges
    }
}

/// Regras built-in de reescrita.
pub fn commutativity_add() -> RewriteRule {
    RewriteRule::new(
        "commutativity_add",
        Pattern::Add(
            Box::new(Pattern::Wildcard("a".to_string())),
            Box::new(Pattern::Wildcard("b".to_string())),
        ),
        Pattern::Add(
            Box::new(Pattern::Wildcard("b".to_string())),
            Box::new(Pattern::Wildcard("a".to_string())),
        ),
    )
}

pub fn commutativity_mul() -> RewriteRule {
    RewriteRule::new(
        "commutativity_mul",
        Pattern::Mul(
            Box::new(Pattern::Wildcard("a".to_string())),
            Box::new(Pattern::Wildcard("b".to_string())),
        ),
        Pattern::Mul(
            Box::new(Pattern::Wildcard("b".to_string())),
            Box::new(Pattern::Wildcard("a".to_string())),
        ),
    )
}

pub fn associativity_add() -> RewriteRule {
    RewriteRule::new(
        "associativity_add",
        Pattern::Add(
            Box::new(Pattern::Add(
                Box::new(Pattern::Wildcard("a".to_string())),
                Box::new(Pattern::Wildcard("b".to_string())),
            )),
            Box::new(Pattern::Wildcard("c".to_string())),
        ),
        Pattern::Add(
            Box::new(Pattern::Wildcard("a".to_string())),
            Box::new(Pattern::Add(
                Box::new(Pattern::Wildcard("b".to_string())),
                Box::new(Pattern::Wildcard("c".to_string())),
            )),
        ),
    )
}

pub fn identity_add() -> RewriteRule {
    RewriteRule::new(
        "identity_add",
        Pattern::Add(
            Box::new(Pattern::Wildcard("x".to_string())),
            Box::new(Pattern::Const(0)),
        ),
        Pattern::Wildcard("x".to_string()),
    )
}

pub fn distributivity_mul_add() -> RewriteRule {
    RewriteRule::new(
        "distributivity_mul_add",
        Pattern::Mul(
            Box::new(Pattern::Wildcard("a".to_string())),
            Box::new(Pattern::Add(
                Box::new(Pattern::Wildcard("b".to_string())),
                Box::new(Pattern::Wildcard("c".to_string())),
            )),
        ),
        Pattern::Add(
            Box::new(Pattern::Mul(
                Box::new(Pattern::Wildcard("a".to_string())),
                Box::new(Pattern::Wildcard("b".to_string())),
            )),
            Box::new(Pattern::Mul(
                Box::new(Pattern::Wildcard("a".to_string())),
                Box::new(Pattern::Wildcard("c".to_string())),
            )),
        ),
    )
}

/// Coleção padrão de regras built-in.
pub fn default_rules() -> Vec<RewriteRule> {
    vec![
        commutativity_add(),
        commutativity_mul(),
        associativity_add(),
        identity_add(),
        distributivity_mul_add(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::Expr;

    #[test]
    fn commutativity_applies() {
        let mut g = EGraph::new();
        let expr = Expr::Add(
            Box::new(Expr::Var("a".to_string())),
            Box::new(Expr::Var("b".to_string())),
        );
        let id = g.add_expr(&expr);
        g.rebuild();

        let rule = commutativity_add();
        let merges = rule.apply(&mut g);
        g.rebuild();

        assert!(!merges.is_empty());
        let root = g.find(id);
        let class = g.get_class(root).unwrap();
        // Deve conter tanto Add(a,b) quanto Add(b,a)
        assert_eq!(class.nodes.len(), 2);
    }

    #[test]
    fn identity_simplifies() {
        let mut g = EGraph::new();
        let expr = Expr::Add(
            Box::new(Expr::Var("x".to_string())),
            Box::new(Expr::Const(0)),
        );
        let id = g.add_expr(&expr);
        g.rebuild();

        let rule = identity_add();
        let merges = rule.apply(&mut g);
        g.rebuild();

        assert!(!merges.is_empty());
        let x_id = g.add_expr(&Expr::Var("x".to_string()));
        assert_eq!(g.find(id), g.find(x_id));
    }

    #[test]
    fn associativity_reassociates() {
        let mut g = EGraph::new();
        let expr = Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Var("b".to_string())),
            )),
            Box::new(Expr::Var("c".to_string())),
        );
        let id = g.add_expr(&expr);
        g.rebuild();

        let rule = associativity_add();
        let merges = rule.apply(&mut g);
        g.rebuild();

        assert!(!merges.is_empty());
        let root = g.find(id);
        let class = g.get_class(root).unwrap();
        assert!(class.nodes.len() >= 2);
    }

    #[test]
    fn distributivity_expands() {
        let mut g = EGraph::new();
        let expr = Expr::Mul(
            Box::new(Expr::Var("a".to_string())),
            Box::new(Expr::Add(
                Box::new(Expr::Var("b".to_string())),
                Box::new(Expr::Var("c".to_string())),
            )),
        );
        let id = g.add_expr(&expr);
        g.rebuild();

        let rule = distributivity_mul_add();
        let merges = rule.apply(&mut g);
        g.rebuild();

        assert!(!merges.is_empty());
        let root = g.find(id);
        let class = g.get_class(root).unwrap();
        assert!(class.nodes.len() >= 2);
    }
}
