//! Bridge entre arreio-supercompile e arreio-eqsat (Equality Saturation).
//!
//! Converte expressões do IR do supercompilador para o e-graph do eqsat,
//! aplica regras de reescrita, e extrai a expressão de menor custo.

use crate::program::Expr as ScExpr;
use arreio_eqsat::egraph::EGraph;
use arreio_eqsat::language::Expr as EqExpr;
use arreio_eqsat::rewrite_rule::{Pattern, RewriteRule};
use arreio_eqsat::saturation_engine::SaturationEngine;

/// Converte uma expressão do supercompilador para o formato do eqsat.
/// Apenas o subconjunto compatível é convertido (Var, Const, Add, Mul).
/// Expressões If/Let/Call não são otimizáveis pelo eqsat atual.
fn to_eqsat_expr(expr: &ScExpr) -> Option<EqExpr> {
    match expr {
        ScExpr::Var(v) => Some(EqExpr::Var(v.clone())),
        ScExpr::Const(c) => Some(EqExpr::Const(*c)),
        ScExpr::Add(a, b) => {
            let la = to_eqsat_expr(a)?;
            let lb = to_eqsat_expr(b)?;
            Some(EqExpr::Add(Box::new(la), Box::new(lb)))
        }
        ScExpr::Mul(a, b) => {
            let la = to_eqsat_expr(a)?;
            let lb = to_eqsat_expr(b)?;
            Some(EqExpr::Mul(Box::new(la), Box::new(lb)))
        }
        // If, Let, Call não são suportados pelo eqsat atual.
        _ => None,
    }
}

/// Converte uma expressão do eqsat de volta para o IR do supercompilador.
fn from_eqsat_expr(expr: &EqExpr) -> ScExpr {
    match expr {
        EqExpr::Var(v) => ScExpr::Var(v.clone()),
        EqExpr::Const(c) => ScExpr::Const(*c),
        EqExpr::Add(a, b) => {
            ScExpr::Add(Box::new(from_eqsat_expr(a)), Box::new(from_eqsat_expr(b)))
        }
        EqExpr::Mul(a, b) => {
            ScExpr::Mul(Box::new(from_eqsat_expr(a)), Box::new(from_eqsat_expr(b)))
        }
        EqExpr::Neg(e) => {
            // Representa -x como (0 - x) no IR do supercompilador.
            ScExpr::Add(
                Box::new(ScExpr::Const(0)),
                Box::new(ScExpr::Mul(
                    Box::new(ScExpr::Const(-1)),
                    Box::new(from_eqsat_expr(e)),
                )),
            )
        }
    }
}

/// Regras de reescrita aritméticas padrão.
fn default_rules() -> Vec<RewriteRule> {
    let x = || Pattern::Wildcard("x".into());
    let y = || Pattern::Wildcard("y".into());
    let z = || Pattern::Wildcard("z".into());
    let c = |v: i64| Pattern::Const(v);

    vec![
        // x + 0 => x
        RewriteRule::new("add_zero", Pattern::Add(Box::new(x()), Box::new(c(0))), x()),
        // 0 + x => x
        RewriteRule::new("zero_add", Pattern::Add(Box::new(c(0)), Box::new(x())), x()),
        // x * 0 => 0
        RewriteRule::new(
            "mul_zero",
            Pattern::Mul(Box::new(x()), Box::new(c(0))),
            c(0),
        ),
        // 0 * x => 0
        RewriteRule::new(
            "zero_mul",
            Pattern::Mul(Box::new(c(0)), Box::new(x())),
            c(0),
        ),
        // x * 1 => x
        RewriteRule::new("mul_one", Pattern::Mul(Box::new(x()), Box::new(c(1))), x()),
        // 1 * x => x
        RewriteRule::new("one_mul", Pattern::Mul(Box::new(c(1)), Box::new(x())), x()),
        // x + x => 2 * x
        RewriteRule::new(
            "add_same",
            Pattern::Add(Box::new(x()), Box::new(x())),
            Pattern::Mul(Box::new(c(2)), Box::new(x())),
        ),
        // (x + y) + z => x + (y + z)  (associatividade)
        RewriteRule::new(
            "add_assoc",
            Pattern::Add(
                Box::new(Pattern::Add(Box::new(x()), Box::new(y()))),
                Box::new(z()),
            ),
            Pattern::Add(
                Box::new(x()),
                Box::new(Pattern::Add(Box::new(y()), Box::new(z()))),
            ),
        ),
        // (x * y) * z => x * (y * z)  (associatividade)
        RewriteRule::new(
            "mul_assoc",
            Pattern::Mul(
                Box::new(Pattern::Mul(Box::new(x()), Box::new(y()))),
                Box::new(z()),
            ),
            Pattern::Mul(
                Box::new(x()),
                Box::new(Pattern::Mul(Box::new(y()), Box::new(z()))),
            ),
        ),
        // x * (y + z) => (x * y) + (x * z)  (distributividade)
        RewriteRule::new(
            "mul_add_dist",
            Pattern::Mul(
                Box::new(x()),
                Box::new(Pattern::Add(Box::new(y()), Box::new(z()))),
            ),
            Pattern::Add(
                Box::new(Pattern::Mul(Box::new(x()), Box::new(y()))),
                Box::new(Pattern::Mul(Box::new(x()), Box::new(z()))),
            ),
        ),
    ]
}

/// Otimiza uma expressão do supercompilador via equality saturation.
///
/// Retorna `Some(otimizado)` se a expressão for convertível e o eqsat produzir
/// um resultado; `None` caso contrário.
pub fn optimize_expr(expr: &ScExpr) -> Option<ScExpr> {
    let eq_expr = to_eqsat_expr(expr)?;

    // Constrói e-graph a partir da expressão.
    let mut egraph = EGraph::new();
    let root = egraph.add_expr(&eq_expr);

    // Executa saturação com regras padrão.
    let rules = default_rules();
    let mut engine = SaturationEngine::new(egraph, rules, 10);
    let _saturated = engine.run().ok()?;

    // Extrai expressão de menor custo (menor número de nós).
    let best = engine.extract_best(root, |_node| 1).ok()?;

    Some(from_eqsat_expr(&best))
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_zero_eliminated() {
        let expr = ScExpr::Add(
            Box::new(ScExpr::Var("x".into())),
            Box::new(ScExpr::Const(0)),
        );
        let opt = optimize_expr(&expr).unwrap();
        assert_eq!(opt, ScExpr::Var("x".into()));
    }

    #[test]
    fn mul_one_eliminated() {
        let expr = ScExpr::Mul(
            Box::new(ScExpr::Const(1)),
            Box::new(ScExpr::Var("y".into())),
        );
        let opt = optimize_expr(&expr).unwrap();
        assert_eq!(opt, ScExpr::Var("y".into()));
    }

    #[test]
    fn unsupported_expr_returns_none() {
        let expr = ScExpr::If(
            Box::new(ScExpr::Const(1)),
            Box::new(ScExpr::Const(2)),
            Box::new(ScExpr::Const(3)),
        );
        assert!(optimize_expr(&expr).is_none());
    }
}
