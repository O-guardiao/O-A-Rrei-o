use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Representação de um programa simplificado para supercompilação.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Expr {
    Var(String),
    Const(i64),
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    If(Box<Expr>, Box<Expr>, Box<Expr>), // cond, then, else
    Let(String, Box<Expr>, Box<Expr>),   // name, value, body
    Call(String, Vec<Expr>),             // function name, args
}

/// Definição de função.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
}

/// Programa completo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub functions: Vec<Function>,
    pub entry: Expr,
}

impl Expr {
    /// Retorna true se a expressão é uma constante.
    pub fn is_const(&self) -> bool {
        matches!(self, Expr::Const(_))
    }

    /// Retorna true se a expressão é uma variável.
    pub fn is_var(&self) -> bool {
        matches!(self, Expr::Var(_))
    }

    /// Tenta obter o valor constante, se for Const.
    pub fn as_const(&self) -> Option<i64> {
        match self {
            Expr::Const(v) => Some(*v),
            _ => None,
        }
    }

    /// Conta o número aproximado de nós na AST.
    pub fn size(&self) -> usize {
        match self {
            Expr::Var(_) | Expr::Const(_) => 1,
            Expr::Add(a, b) | Expr::Mul(a, b) => 1 + a.size() + b.size(),
            Expr::If(c, t, e) => 1 + c.size() + t.size() + e.size(),
            Expr::Let(_, v, b) => 1 + v.size() + b.size(),
            Expr::Call(_, args) => 1 + args.iter().map(|a| a.size()).sum::<usize>(),
        }
    }

    /// Substitui variáveis por expressões de acordo com o mapeamento.
    pub fn substitute(&self, env: &HashMap<String, Expr>) -> Expr {
        match self {
            Expr::Var(name) => {
                if let Some(e) = env.get(name) {
                    e.clone()
                } else {
                    self.clone()
                }
            }
            Expr::Const(_) => self.clone(),
            Expr::Add(a, b) => Expr::Add(Box::new(a.substitute(env)), Box::new(b.substitute(env))),
            Expr::Mul(a, b) => Expr::Mul(Box::new(a.substitute(env)), Box::new(b.substitute(env))),
            Expr::If(c, t, e) => Expr::If(
                Box::new(c.substitute(env)),
                Box::new(t.substitute(env)),
                Box::new(e.substitute(env)),
            ),
            Expr::Let(name, val, body) => {
                let mut env2 = env.clone();
                env2.remove(name);
                Expr::Let(
                    name.clone(),
                    Box::new(val.substitute(env)),
                    Box::new(body.substitute(&env2)),
                )
            }
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| a.substitute(env)).collect(),
            ),
        }
    }

    /// Coleta todos os nomes de variáveis livres.
    pub fn free_vars(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        self.collect_free_vars(&mut set, &mut HashSet::new());
        set
    }

    fn collect_free_vars(&self, free: &mut HashSet<String>, bound: &mut HashSet<String>) {
        match self {
            Expr::Var(name) => {
                if !bound.contains(name) {
                    free.insert(name.clone());
                }
            }
            Expr::Const(_) => {}
            Expr::Add(a, b) | Expr::Mul(a, b) => {
                a.collect_free_vars(free, bound);
                b.collect_free_vars(free, bound);
            }
            Expr::If(c, t, e) => {
                c.collect_free_vars(free, bound);
                t.collect_free_vars(free, bound);
                e.collect_free_vars(free, bound);
            }
            Expr::Let(name, val, body) => {
                val.collect_free_vars(free, bound);
                bound.insert(name.clone());
                body.collect_free_vars(free, bound);
                bound.remove(name);
            }
            Expr::Call(_, args) => {
                for a in args {
                    a.collect_free_vars(free, bound);
                }
            }
        }
    }
}
