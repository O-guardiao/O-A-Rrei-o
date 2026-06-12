use crate::program::{Expr, Function, Program};
use std::collections::HashMap;

/// Motor de supercompilação.
pub struct Supercompiler {
    max_depth: usize,
    memo: HashMap<Expr, Expr>,
    new_functions: Vec<Function>,
    func_counter: usize,
}

impl Supercompiler {
    pub fn new() -> Self {
        Self {
            max_depth: 64,
            memo: HashMap::new(),
            new_functions: Vec::new(),
            func_counter: 0,
        }
    }

    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    /// Supercompila um programa: especializa com base em entradas conhecidas.
    pub fn supercompile(
        &mut self,
        program: &Program,
        known_inputs: &HashMap<String, i64>,
    ) -> Program {
        // Converte entradas conhecidas (i64) para Expr::Const
        let env: HashMap<String, Expr> = known_inputs
            .iter()
            .map(|(k, v)| (k.clone(), Expr::Const(*v)))
            .collect();

        let specialized_entry = program.entry.substitute(&env);
        let mut driven_entry = self.drive(&specialized_entry);
        driven_entry = self.residualize(&driven_entry);

        // Processa funções existentes
        let mut functions = Vec::new();
        for f in &program.functions {
            let spec_body = f.body.substitute(&env);
            let mut driven_body = self.drive(&spec_body);
            driven_body = self.residualize(&driven_body);
            functions.push(Function {
                name: f.name.clone(),
                params: f.params.clone(),
                body: driven_body,
            });
        }

        // Adiciona funções geradas pelo folding
        functions.extend(self.new_functions.clone());

        Program {
            functions,
            entry: driven_entry,
        }
    }

    /// Driving: executa passos de redução simbólica.
    pub fn drive(&mut self, expr: &Expr) -> Expr {
        self.drive_with_depth(expr, 0)
    }

    fn drive_with_depth(&mut self, expr: &Expr, depth: usize) -> Expr {
        if depth > self.max_depth {
            return expr.clone();
        }

        if let Some(result) = self.memo.get(expr) {
            return result.clone();
        }

        let result = match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => {
                let da = self.drive_with_depth(a, depth + 1);
                let db = self.drive_with_depth(b, depth + 1);
                match (da.as_const(), db.as_const()) {
                    (Some(av), Some(bv)) => Expr::Const(av + bv),
                    _ => Expr::Add(Box::new(da), Box::new(db)),
                }
            }
            Expr::Mul(a, b) => {
                let da = self.drive_with_depth(a, depth + 1);
                let db = self.drive_with_depth(b, depth + 1);
                match (da.as_const(), db.as_const()) {
                    (Some(av), Some(bv)) => Expr::Const(av * bv),
                    _ => {
                        // Otimizações algébricas
                        if da.as_const() == Some(0) || db.as_const() == Some(0) {
                            Expr::Const(0)
                        } else if da.as_const() == Some(1) {
                            db
                        } else if db.as_const() == Some(1) {
                            da
                        } else {
                            Expr::Mul(Box::new(da), Box::new(db))
                        }
                    }
                }
            }
            Expr::If(c, t, e) => {
                let dc = self.drive_with_depth(c, depth + 1);
                match dc.as_const() {
                    Some(0) => self.drive_with_depth(e, depth + 1),
                    Some(_) => self.drive_with_depth(t, depth + 1),
                    None => Expr::If(
                        Box::new(dc),
                        Box::new(self.drive_with_depth(t, depth + 1)),
                        Box::new(self.drive_with_depth(e, depth + 1)),
                    ),
                }
            }
            Expr::Let(name, val, body) => {
                let dv = self.drive_with_depth(val, depth + 1);
                if let Some(v) = dv.as_const() {
                    let mut env = HashMap::new();
                    env.insert(name.clone(), Expr::Const(v));
                    let substituted = body.substitute(&env);
                    self.drive_with_depth(&substituted, depth + 1)
                } else {
                    Expr::Let(
                        name.clone(),
                        Box::new(dv),
                        Box::new(self.drive_with_depth(body, depth + 1)),
                    )
                }
            }
            Expr::Call(name, args) => {
                let dargs: Vec<Expr> = args
                    .iter()
                    .map(|a| self.drive_with_depth(a, depth + 1))
                    .collect();
                Expr::Call(name.clone(), dargs)
            }
        };

        self.memo.insert(expr.clone(), result.clone());
        result
    }

    /// Folding: identifica padrões recursivos e cria novas funções.
    /// Retorna as funções geradas pelo folding.
    pub fn generated_functions(&self) -> &[Function] {
        &self.new_functions
    }

    pub fn fold(&mut self, expr: &Expr, history: &[Expr]) -> Expr {
        // Verifica se expr é similar a alguma expressão no histórico
        for old in history.iter() {
            if self.is_similar(old, expr) {
                let generalized = self.generalize(old, expr);
                let func_name = format!("__gen_{}", self.func_counter);
                self.func_counter += 1;

                // Coleta variáveis livres da expressão generalizada
                let fvs: Vec<String> = generalized.free_vars().into_iter().collect();
                let func = Function {
                    name: func_name.clone(),
                    params: fvs.clone(),
                    body: generalized.clone(),
                };
                self.new_functions.push(func);

                // Retorna chamada à nova função
                let call_args: Vec<Expr> = fvs.into_iter().map(Expr::Var).collect();
                return Expr::Call(func_name, call_args);
            }
        }

        // Não encontrou similaridade; retorna a expressão processada recursivamente
        match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => Expr::Add(
                Box::new(self.fold(a, history)),
                Box::new(self.fold(b, history)),
            ),
            Expr::Mul(a, b) => Expr::Mul(
                Box::new(self.fold(a, history)),
                Box::new(self.fold(b, history)),
            ),
            Expr::If(c, t, e) => {
                let mut new_history = history.to_vec();
                new_history.push(expr.clone());
                Expr::If(
                    Box::new(self.fold(c, history)),
                    Box::new(self.fold(t, &new_history)),
                    Box::new(self.fold(e, &new_history)),
                )
            }
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(self.fold(val, history)),
                Box::new(self.fold(body, history)),
            ),
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| self.fold(a, history)).collect(),
            ),
        }
    }

    /// Verifica se duas expressões são "similares" (mesma estrutura, possivelmente diferentes constantes).
    fn is_similar(&self, e1: &Expr, e2: &Expr) -> bool {
        match (e1, e2) {
            (Expr::Var(a), Expr::Var(b)) => a == b,
            (Expr::Const(_), Expr::Const(_)) => true,
            (Expr::Add(a1, b1), Expr::Add(a2, b2)) | (Expr::Mul(a1, b1), Expr::Mul(a2, b2)) => {
                self.is_similar(a1, a2) && self.is_similar(b1, b2)
            }
            (Expr::If(c1, t1, e1), Expr::If(c2, t2, e2)) => {
                self.is_similar(c1, c2) && self.is_similar(t1, t2) && self.is_similar(e1, e2)
            }
            (Expr::Let(n1, v1, b1), Expr::Let(n2, v2, b2)) => {
                n1 == n2 && self.is_similar(v1, v2) && self.is_similar(b1, b2)
            }
            (Expr::Call(n1, a1), Expr::Call(n2, a2)) => {
                n1 == n2
                    && a1.len() == a2.len()
                    && a1.iter().zip(a2.iter()).all(|(x, y)| self.is_similar(x, y))
            }
            _ => false,
        }
    }

    /// Generalization: quando loop detectado, generaliza expressões.
    pub fn generalize(&self, e1: &Expr, e2: &Expr) -> Expr {
        match (e1, e2) {
            (Expr::Const(a), Expr::Const(b)) => {
                if a == b {
                    Expr::Const(*a)
                } else {
                    // Diferentes constantes: generaliza para variável
                    Expr::Var("__g".to_string())
                }
            }
            (Expr::Var(a), Expr::Var(b)) => {
                if a == b {
                    Expr::Var(a.clone())
                } else {
                    Expr::Var("__g".to_string())
                }
            }
            (Expr::Add(a1, b1), Expr::Add(a2, b2)) => Expr::Add(
                Box::new(self.generalize(a1, a2)),
                Box::new(self.generalize(b1, b2)),
            ),
            (Expr::Mul(a1, b1), Expr::Mul(a2, b2)) => Expr::Mul(
                Box::new(self.generalize(a1, a2)),
                Box::new(self.generalize(b1, b2)),
            ),
            (Expr::If(c1, t1, e1), Expr::If(c2, t2, e2)) => Expr::If(
                Box::new(self.generalize(c1, c2)),
                Box::new(self.generalize(t1, t2)),
                Box::new(self.generalize(e1, e2)),
            ),
            (Expr::Let(n1, v1, b1), Expr::Let(n2, v2, b2)) => {
                let name = if n1 == n2 {
                    n1.clone()
                } else {
                    "__g_name".to_string()
                };
                Expr::Let(
                    name,
                    Box::new(self.generalize(v1, v2)),
                    Box::new(self.generalize(b1, b2)),
                )
            }
            (Expr::Call(n1, a1), Expr::Call(n2, a2)) => {
                let name = if n1 == n2 {
                    n1.clone()
                } else {
                    "__g_fn".to_string()
                };
                let args: Vec<Expr> = a1
                    .iter()
                    .zip(a2.iter())
                    .map(|(x, y)| self.generalize(x, y))
                    .collect();
                Expr::Call(name, args)
            }
            // Estruturas diferentes: generaliza para variável
            _ => Expr::Var("__g".to_string()),
        }
    }

    /// Residualization: gera programa residual otimizado.
    pub fn residualize(&self, expr: &Expr) -> Expr {
        match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => {
                let ra = self.residualize(a);
                let rb = self.residualize(b);
                match (ra.as_const(), rb.as_const()) {
                    (Some(av), Some(bv)) => Expr::Const(av + bv),
                    _ => Expr::Add(Box::new(ra), Box::new(rb)),
                }
            }
            Expr::Mul(a, b) => {
                let ra = self.residualize(a);
                let rb = self.residualize(b);
                match (ra.as_const(), rb.as_const()) {
                    (Some(av), Some(bv)) => Expr::Const(av * bv),
                    _ => Expr::Mul(Box::new(ra), Box::new(rb)),
                }
            }
            Expr::If(c, t, e) => {
                let rc = self.residualize(c);
                match rc.as_const() {
                    Some(0) => self.residualize(e),
                    Some(_) => self.residualize(t),
                    None => Expr::If(
                        Box::new(rc),
                        Box::new(self.residualize(t)),
                        Box::new(self.residualize(e)),
                    ),
                }
            }
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(self.residualize(val)),
                Box::new(self.residualize(body)),
            ),
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| self.residualize(a)).collect(),
            ),
        }
    }
}

impl Default for Supercompiler {
    fn default() -> Self {
        Self::new()
    }
}
