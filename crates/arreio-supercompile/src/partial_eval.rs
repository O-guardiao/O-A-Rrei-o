use crate::program::{Expr, Function, Program};
use std::collections::HashMap;

/// Avaliador parcial — avalia o que pode em tempo de compilação.
pub struct PartialEvaluator;

impl PartialEvaluator {
    pub fn new() -> Self {
        Self
    }

    /// Avalia expressão substituindo variáveis conhecidas.
    pub fn evaluate(&self, expr: &Expr, env: &HashMap<String, i64>) -> Expr {
        match expr {
            Expr::Var(name) => {
                if let Some(v) = env.get(name) {
                    Expr::Const(*v)
                } else {
                    expr.clone()
                }
            }
            Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => {
                let ae = self.evaluate(a, env);
                let be = self.evaluate(b, env);
                match (ae.as_const(), be.as_const()) {
                    (Some(av), Some(bv)) => Expr::Const(av + bv),
                    _ => Expr::Add(Box::new(ae), Box::new(be)),
                }
            }
            Expr::Mul(a, b) => {
                let ae = self.evaluate(a, env);
                let be = self.evaluate(b, env);
                match (ae.as_const(), be.as_const()) {
                    (Some(av), Some(bv)) => Expr::Const(av * bv),
                    _ => Expr::Mul(Box::new(ae), Box::new(be)),
                }
            }
            Expr::If(c, t, e) => {
                let ce = self.evaluate(c, env);
                match ce.as_const() {
                    Some(0) => self.evaluate(e, env),
                    Some(_) => self.evaluate(t, env),
                    None => Expr::If(
                        Box::new(ce),
                        Box::new(self.evaluate(t, env)),
                        Box::new(self.evaluate(e, env)),
                    ),
                }
            }
            Expr::Let(name, val, body) => {
                let ve = self.evaluate(val, env);
                if let Some(v) = ve.as_const() {
                    let mut env2 = env.clone();
                    env2.insert(name.clone(), v);
                    self.evaluate(body, &env2)
                } else {
                    Expr::Let(
                        name.clone(),
                        Box::new(ve),
                        Box::new(self.evaluate(body, env)),
                    )
                }
            }
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| self.evaluate(a, env)).collect(),
            ),
        }
    }

    /// Propaga constantes em uma expressão.
    pub fn constant_propagation(&self, expr: &Expr) -> Expr {
        self.evaluate(expr, &HashMap::new())
    }

    /// Elimina código morto (ramos If com condição constante).
    pub fn dead_code_elimination(&self, expr: &Expr) -> Expr {
        match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => Expr::Add(
                Box::new(self.dead_code_elimination(a)),
                Box::new(self.dead_code_elimination(b)),
            ),
            Expr::Mul(a, b) => Expr::Mul(
                Box::new(self.dead_code_elimination(a)),
                Box::new(self.dead_code_elimination(b)),
            ),
            Expr::If(c, t, e) => {
                let cd = self.dead_code_elimination(c);
                let td = self.dead_code_elimination(t);
                let ed = self.dead_code_elimination(e);
                match cd.as_const() {
                    Some(0) => ed,
                    Some(_) => td,
                    None => Expr::If(Box::new(cd), Box::new(td), Box::new(ed)),
                }
            }
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(self.dead_code_elimination(val)),
                Box::new(self.dead_code_elimination(body)),
            ),
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| self.dead_code_elimination(a)).collect(),
            ),
        }
    }

    /// Inline de funções pequenas (body <= max_body_size).
    pub fn inline_functions(&self, program: &Program, max_body_size: usize) -> Program {
        let mut new_entry = program.entry.clone();
        let mut new_functions: Vec<Function> = Vec::new();

        let func_map: HashMap<String, &Function> = program
            .functions
            .iter()
            .map(|f| (f.name.clone(), f))
            .collect();

        // Inline na entry
        new_entry = self.inline_expr(&new_entry, &func_map, max_body_size);

        // Inline nos corpos das funções
        for f in &program.functions {
            let new_body = self.inline_expr(&f.body, &func_map, max_body_size);
            new_functions.push(Function {
                name: f.name.clone(),
                params: f.params.clone(),
                body: new_body,
            });
        }

        Program {
            functions: new_functions,
            entry: new_entry,
        }
    }

    fn inline_expr(
        &self,
        expr: &Expr,
        func_map: &HashMap<String, &Function>,
        max_body_size: usize,
    ) -> Expr {
        match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => Expr::Add(
                Box::new(self.inline_expr(a, func_map, max_body_size)),
                Box::new(self.inline_expr(b, func_map, max_body_size)),
            ),
            Expr::Mul(a, b) => Expr::Mul(
                Box::new(self.inline_expr(a, func_map, max_body_size)),
                Box::new(self.inline_expr(b, func_map, max_body_size)),
            ),
            Expr::If(c, t, e) => Expr::If(
                Box::new(self.inline_expr(c, func_map, max_body_size)),
                Box::new(self.inline_expr(t, func_map, max_body_size)),
                Box::new(self.inline_expr(e, func_map, max_body_size)),
            ),
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(self.inline_expr(val, func_map, max_body_size)),
                Box::new(self.inline_expr(body, func_map, max_body_size)),
            ),
            Expr::Call(name, args) => {
                let inlined_args: Vec<Expr> = args
                    .iter()
                    .map(|a| self.inline_expr(a, func_map, max_body_size))
                    .collect();

                if let Some(func) = func_map.get(name) {
                    if func.body.size() <= max_body_size {
                        // Inline: substituir parâmetros pelos argumentos
                        let mut subst = HashMap::new();
                        for (p, a) in func.params.iter().zip(inlined_args.iter()) {
                            subst.insert(p.clone(), a.clone());
                        }
                        return func.body.substitute(&subst);
                    }
                }
                Expr::Call(name.clone(), inlined_args)
            }
        }
    }
}

impl Default for PartialEvaluator {
    fn default() -> Self {
        Self::new()
    }
}
