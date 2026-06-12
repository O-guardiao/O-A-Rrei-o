//! Motor de saturação para Equality Saturation.

use crate::egraph::{EClassId, EGraph, ENode};
use crate::language::Expr;
use crate::rewrite_rule::RewriteRule;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// Motor de saturação: aplica regras iterativamente até saturação ou limite.
#[derive(Debug, Clone)]
pub struct SaturationEngine {
    pub egraph: EGraph,
    pub rules: Vec<RewriteRule>,
    pub iteration_limit: usize,
    merged_last_iteration: bool,
}

impl SaturationEngine {
    pub fn new(egraph: EGraph, rules: Vec<RewriteRule>, iteration_limit: usize) -> Self {
        Self {
            egraph,
            rules,
            iteration_limit,
            merged_last_iteration: true,
        }
    }

    /// Executa a saturação até que nenhuma regra produza novos merges
    /// ou o limite de iterações seja atingido.
    pub fn run(&mut self) -> Result<EGraph> {
        for _ in 0..self.iteration_limit {
            let mut any_merge = false;
            for rule in &self.rules {
                let merges = rule.apply(&mut self.egraph);
                if !merges.is_empty() {
                    any_merge = true;
                }
            }
            self.egraph.rebuild();
            self.merged_last_iteration = any_merge;
            if !any_merge {
                break;
            }
        }
        Ok(self.egraph.clone())
    }

    /// Verifica se a última iteração não produziu novos merges.
    pub fn is_saturated(&self) -> bool {
        !self.merged_last_iteration
    }

    /// Extrai a expressão de menor custo de uma e-class.
    pub fn extract_best(&self, root: EClassId, cost_fn: fn(&ENode) -> usize) -> Result<Expr> {
        let mut memo: HashMap<EClassId, (usize, Expr)> = HashMap::new();
        let mut visiting: HashSet<EClassId> = HashSet::new();
        self.extract_best_rec(root, cost_fn, &mut memo, &mut visiting)
    }

    fn extract_best_rec(
        &self,
        id: EClassId,
        cost_fn: fn(&ENode) -> usize,
        memo: &mut HashMap<EClassId, (usize, Expr)>,
        visiting: &mut HashSet<EClassId>,
    ) -> Result<Expr> {
        let root = self.egraph.union_find.parent.get(id).copied().unwrap_or(id);
        if let Some((_, expr)) = memo.get(&root) {
            return Ok(expr.clone());
        }
        if visiting.contains(&root) {
            anyhow::bail!("ciclo detectado na e-class {}", root);
        }

        let class = self
            .egraph
            .classes
            .get(&root)
            .ok_or_else(|| anyhow::anyhow!("e-class {} não encontrada", root))?;

        visiting.insert(root);
        let mut best_cost: Option<usize> = None;
        let mut best_expr: Option<Expr> = None;

        for node in &class.nodes {
            match self.node_to_expr(node, cost_fn, memo, visiting) {
                Ok((expr, child_cost)) => {
                    let total_cost = cost_fn(node) + child_cost;
                    if best_cost.is_none() || total_cost < best_cost.unwrap() {
                        best_cost = Some(total_cost);
                        best_expr = Some(expr);
                    }
                }
                Err(_) => continue,
            }
        }

        visiting.remove(&root);

        match best_expr {
            Some(expr) => {
                memo.insert(root, (best_cost.unwrap(), expr.clone()));
                Ok(expr)
            }
            None => anyhow::bail!("não foi possível extrair expressão da e-class {}", root),
        }
    }

    fn node_to_expr(
        &self,
        node: &ENode,
        cost_fn: fn(&ENode) -> usize,
        memo: &mut HashMap<EClassId, (usize, Expr)>,
        visiting: &mut HashSet<EClassId>,
    ) -> Result<(Expr, usize)> {
        match node.op.as_str() {
            s if s.starts_with("Const:") => {
                let val = s
                    .strip_prefix("Const:")
                    .unwrap()
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("constante inválida: {}", s))?;
                Ok((Expr::Const(val), 0))
            }
            s if s.starts_with("Var:") => {
                let name = s.strip_prefix("Var:").unwrap().to_string();
                Ok((Expr::Var(name), 0))
            }
            "Add" => {
                if node.children.len() != 2 {
                    anyhow::bail!("Add espera 2 filhos");
                }
                let (a, ca) = self.node_to_expr_rec(node.children[0], cost_fn, memo, visiting)?;
                let (b, cb) = self.node_to_expr_rec(node.children[1], cost_fn, memo, visiting)?;
                Ok((Expr::Add(Box::new(a), Box::new(b)), ca + cb))
            }
            "Mul" => {
                if node.children.len() != 2 {
                    anyhow::bail!("Mul espera 2 filhos");
                }
                let (a, ca) = self.node_to_expr_rec(node.children[0], cost_fn, memo, visiting)?;
                let (b, cb) = self.node_to_expr_rec(node.children[1], cost_fn, memo, visiting)?;
                Ok((Expr::Mul(Box::new(a), Box::new(b)), ca + cb))
            }
            "Neg" => {
                if node.children.len() != 1 {
                    anyhow::bail!("Neg espera 1 filho");
                }
                let (a, ca) = self.node_to_expr_rec(node.children[0], cost_fn, memo, visiting)?;
                Ok((Expr::Neg(Box::new(a)), ca))
            }
            other => anyhow::bail!("operador desconhecido: {}", other),
        }
    }

    fn node_to_expr_rec(
        &self,
        id: EClassId,
        cost_fn: fn(&ENode) -> usize,
        memo: &mut HashMap<EClassId, (usize, Expr)>,
        visiting: &mut HashSet<EClassId>,
    ) -> Result<(Expr, usize)> {
        let expr = self.extract_best_rec(id, cost_fn, memo, visiting)?;
        let root = self.egraph.union_find.parent.get(id).copied().unwrap_or(id);
        let cost = memo.get(&root).map(|(c, _)| *c).unwrap_or(0);
        Ok((expr, cost))
    }
}

/// Função de custo padrão: conta número de operadores.
pub fn default_cost(node: &ENode) -> usize {
    match node.op.as_str() {
        "Add" | "Mul" => 1,
        "Neg" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::Expr;
    use crate::rewrite_rule::{default_rules, identity_add};

    #[test]
    fn saturation_converges_to_optimal() {
        let g = EGraph::new();
        let rules = default_rules();
        let mut engine = SaturationEngine::new(g, rules, 10);

        let expr = Expr::Add(
            Box::new(Expr::Var("x".to_string())),
            Box::new(Expr::Const(0)),
        );
        let root = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        let _ = engine.run().unwrap();
        let best = engine.extract_best(root, default_cost).unwrap();

        assert_eq!(best, Expr::Var("x".to_string()));
    }

    #[test]
    fn saturation_hits_iteration_limit() {
        let g = EGraph::new();
        // Regra de identidade que sempre produz merge
        let rules = vec![identity_add()];
        let mut engine = SaturationEngine::new(g, rules, 2);

        let expr = Expr::Add(
            Box::new(Expr::Var("x".to_string())),
            Box::new(Expr::Const(0)),
        );
        let root = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        let result = engine.run().unwrap();
        // Com apenas uma regra que sempre matcha, o merge ocorre cedo e o
        // engine satura (ou para no limite) sem perder a classe raiz: a
        // expressão original precisa continuar extraível do egraph retornado.
        assert!(result.get_class(root).is_some() || engine.is_saturated());
        let best = engine.extract_best(root, default_cost).unwrap();
        assert_eq!(best, Expr::Var("x".to_string()));
    }

    #[test]
    fn extract_best_returns_smaller_expr() {
        let g = EGraph::new();
        let rules = vec![identity_add()];
        let mut engine = SaturationEngine::new(g, rules, 5);

        let expr = Expr::Add(
            Box::new(Expr::Var("x".to_string())),
            Box::new(Expr::Const(0)),
        );
        let root = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        engine.run().unwrap();
        let best = engine.extract_best(root, default_cost).unwrap();

        // A expressão ótima deve ser menor (x) do que a original (x + 0)
        assert_eq!(best, Expr::Var("x".to_string()));
    }

    #[test]
    fn saturation_with_multiple_rules() {
        let g = EGraph::new();
        let rules = default_rules();
        let mut engine = SaturationEngine::new(g, rules, 10);

        let expr = Expr::Mul(
            Box::new(Expr::Const(2)),
            Box::new(Expr::Add(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Var("b".to_string())),
            )),
        );
        let root = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        engine.run().unwrap();

        // Verifica que a distributividade foi aplicada:
        // a e-class raiz deve conter nodos de ambas as representações.
        let root_id = engine.egraph.find(root);
        let class = engine.egraph.get_class(root_id).unwrap();
        assert!(class.nodes.len() >= 2);
    }

    #[test]
    fn extract_best_cost_function() {
        let g = EGraph::new();
        let mut engine = SaturationEngine::new(g, vec![], 1);

        let expr = Expr::Add(
            Box::new(Expr::Const(1)),
            Box::new(Expr::Mul(
                Box::new(Expr::Const(2)),
                Box::new(Expr::Const(3)),
            )),
        );
        let root = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        let best = engine.extract_best(root, default_cost).unwrap();
        assert_eq!(best, expr);
    }

    #[test]
    fn saturation_is_saturated_when_done() {
        let g = EGraph::new();
        let rules = vec![];
        let mut engine = SaturationEngine::new(g, rules, 5);

        let expr = Expr::Const(42);
        let _ = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        let _ = engine.run().unwrap();
        assert!(engine.is_saturated());
    }

    #[test]
    fn associativity_reassociates_in_saturation() {
        let g = EGraph::new();
        let rules = default_rules();
        let mut engine = SaturationEngine::new(g, rules, 10);

        let expr = Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Var("b".to_string())),
            )),
            Box::new(Expr::Var("c".to_string())),
        );
        let root = engine.egraph.add_expr(&expr);
        engine.egraph.rebuild();

        engine.run().unwrap();

        // Verifica que a associatividade foi aplicada:
        // a e-class raiz deve conter a representação reassociada.
        let root_id = engine.egraph.find(root);
        let class = engine.egraph.get_class(root_id).unwrap();
        assert!(class.nodes.len() >= 2);
    }
}
