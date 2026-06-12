use crate::program::{Expr, Function, Program};
use crate::SupercompilePipeline;
use anyhow::Result;
use std::collections::HashSet;

/// Saída otimizada do pipeline pós-geração de código.
#[derive(Debug, Clone, PartialEq)]
pub struct OptimizedOutput {
    pub code: String,
    pub optimizations_applied: Vec<String>,
    pub verification_passed: bool,
    pub original_size: usize,
    pub optimized_size: usize,
}

/// Pipeline integrado de otimização pós-LLM.
pub struct PostGenPipeline {
    pub supercompile: SupercompilePipeline,
    pub supercompile_enabled: bool,
    pub eqsat_enabled: bool,
    pub slice_enabled: bool,
}

impl PostGenPipeline {
    pub fn new() -> Self {
        Self {
            supercompile: SupercompilePipeline,
            supercompile_enabled: true,
            eqsat_enabled: true,
            slice_enabled: true,
        }
    }

    /// Processa código gerado por LLM através do pipeline completo de otimização.
    ///
    /// Etapas:
    /// 1. Program Slicing (remove código morto e irrelevante)
    /// 2. Supercompilation (partial evaluation, driving, folding)
    /// 3. Equality Saturation (e-graphs + reescrita)
    /// 4. Métricas de tamanho
    ///
    /// Nota: verificação formal (Hoare/Abstract Interpretation) é responsabilidade
    /// do caller via ContractEngine — este pipeline não retorna placebo.
    pub fn process(&self, input_code: &str) -> Result<OptimizedOutput> {
        let mut optimizations_applied: Vec<String> = Vec::new();

        let program: Program = serde_json::from_str(input_code)?;
        let original_size = program.entry.size()
            + program
                .functions
                .iter()
                .map(|f| f.body.size())
                .sum::<usize>();

        let mut current = program;

        // 1. Program Slicing: remove código morto e irrelevante.
        if self.slice_enabled {
            current = self.slice_program(&current);
            optimizations_applied.push("program_slicing".to_string());
        }

        // 2. Supercompilation: partial evaluation, driving, folding.
        if self.supercompile_enabled {
            let known = std::collections::HashMap::new();
            current = SupercompilePipeline::run(&current, &known);
            optimizations_applied.push("supercompilation".to_string());
        }

        // 3. Equality Saturation: otimiza via e-graphs e reescrita não-destrutiva.
        if self.eqsat_enabled {
            current = self.apply_eqsat(&current)?;
            optimizations_applied.push("equality_saturation".to_string());
        }

        // 4. Verificação formal NÃO é feita aqui — evita placebo.
        // O caller (symbion_pipeline) deve usar ContractEngine/HoareTriple
        // para verificar invariantes se desejar garantia formal.
        let verification_passed = false;

        let optimized_size = current.entry.size()
            + current
                .functions
                .iter()
                .map(|f| f.body.size())
                .sum::<usize>();

        let code = serde_json::to_string_pretty(&current)?;

        Ok(OptimizedOutput {
            code,
            optimizations_applied,
            verification_passed,
            original_size,
            optimized_size,
        })
    }

    /// Remove funções não alcançáveis a partir da entry e Let bindings não utilizados.
    fn slice_program(&self, program: &Program) -> Program {
        let mut used_functions = HashSet::new();
        Self::collect_calls(&program.entry, &mut used_functions);

        let mut reached_fixed_point = false;
        while !reached_fixed_point {
            let before = used_functions.len();
            for f in &program.functions {
                if used_functions.contains(&f.name) {
                    Self::collect_calls(&f.body, &mut used_functions);
                }
            }
            reached_fixed_point = used_functions.len() == before;
        }

        let filtered_functions: Vec<Function> = program
            .functions
            .iter()
            .filter(|f| used_functions.contains(&f.name))
            .cloned()
            .collect();

        let entry = self.slice_expr(&program.entry);
        let functions: Vec<Function> = filtered_functions
            .into_iter()
            .map(|f| Function {
                name: f.name,
                params: f.params,
                body: self.slice_expr(&f.body),
            })
            .collect();

        Program { functions, entry }
    }

    /// Remove Let bindings cujo valor não é referenciado no corpo.
    fn slice_expr(&self, expr: &Expr) -> Expr {
        match expr {
            Expr::Let(name, val, body) => {
                let sliced_body = self.slice_expr(body);
                let sliced_val = self.slice_expr(val);
                let body_free = sliced_body.free_vars();
                if body_free.contains(name) {
                    Expr::Let(name.clone(), Box::new(sliced_val), Box::new(sliced_body))
                } else {
                    sliced_body
                }
            }
            Expr::Add(a, b) => {
                Expr::Add(Box::new(self.slice_expr(a)), Box::new(self.slice_expr(b)))
            }
            Expr::Mul(a, b) => {
                Expr::Mul(Box::new(self.slice_expr(a)), Box::new(self.slice_expr(b)))
            }
            Expr::If(c, t, e) => Expr::If(
                Box::new(self.slice_expr(c)),
                Box::new(self.slice_expr(t)),
                Box::new(self.slice_expr(e)),
            ),
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| self.slice_expr(a)).collect(),
            ),
            _ => expr.clone(),
        }
    }

    fn collect_calls(expr: &Expr, calls: &mut HashSet<String>) {
        match expr {
            Expr::Call(name, args) => {
                calls.insert(name.clone());
                for a in args {
                    Self::collect_calls(a, calls);
                }
            }
            Expr::Add(a, b) | Expr::Mul(a, b) => {
                Self::collect_calls(a, calls);
                Self::collect_calls(b, calls);
            }
            Expr::If(c, t, e) => {
                Self::collect_calls(c, calls);
                Self::collect_calls(t, calls);
                Self::collect_calls(e, calls);
            }
            Expr::Let(_, val, body) => {
                Self::collect_calls(val, calls);
                Self::collect_calls(body, calls);
            }
            _ => {}
        }
    }

    /// Aplica equality saturation via e-graphs em todo o programa.
    fn apply_eqsat(&self, program: &Program) -> Result<Program> {
        let entry = Self::optimize_expr_eqsat(&program.entry)?;
        let functions: Vec<Function> = program
            .functions
            .iter()
            .map(|f| {
                Ok(Function {
                    name: f.name.clone(),
                    params: f.params.clone(),
                    body: Self::optimize_expr_eqsat(&f.body)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Program { functions, entry })
    }

    /// Otimiza expressão aplicando equality saturation em subexpressões aritméticas.
    fn optimize_expr_eqsat(expr: &Expr) -> Result<Expr> {
        let eqsat = Self::eqsat_optimize_expr(expr);
        let mut current = eqsat;
        let mut changed = true;
        while changed {
            changed = false;
            current = Self::eqsat_normalize_rules(&current, &mut changed);
        }
        Ok(current)
    }

    fn eqsat_optimize_expr(expr: &Expr) -> Expr {
        match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => {
                let na = Self::eqsat_optimize_expr(a);
                let nb = Self::eqsat_optimize_expr(b);
                let combined = Expr::Add(Box::new(na), Box::new(nb));
                Self::eqsat_try_optimize(&combined).unwrap_or(combined)
            }
            Expr::Mul(a, b) => {
                let na = Self::eqsat_optimize_expr(a);
                let nb = Self::eqsat_optimize_expr(b);
                let combined = Expr::Mul(Box::new(na), Box::new(nb));
                Self::eqsat_try_optimize(&combined).unwrap_or(combined)
            }
            Expr::If(c, t, e) => Expr::If(
                Box::new(Self::eqsat_optimize_expr(c)),
                Box::new(Self::eqsat_optimize_expr(t)),
                Box::new(Self::eqsat_optimize_expr(e)),
            ),
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(Self::eqsat_optimize_expr(val)),
                Box::new(Self::eqsat_optimize_expr(body)),
            ),
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter().map(|a| Self::eqsat_optimize_expr(a)).collect(),
            ),
        }
    }

    /// Tenta aplicar equality saturation se a expressão for puramente aritmética.
    fn eqsat_try_optimize(expr: &Expr) -> Option<Expr> {
        let eqsat_expr = Self::to_eqsat_expr(expr)?;
        let mut egraph = arreio_eqsat::EGraph::new();
        let root = egraph.add_expr(&eqsat_expr);
        egraph.rebuild();

        let rules = arreio_eqsat::default_rules();
        let mut engine = arreio_eqsat::SaturationEngine::new(egraph, rules, 10);
        let _ = engine.run().ok()?;
        let best = engine.extract_best(root, arreio_eqsat::default_cost).ok()?;
        Some(Self::from_eqsat_expr(&best))
    }

    /// Converte Expr do supercompile para Expr do eqsat (somente aritmética pura).
    fn to_eqsat_expr(expr: &Expr) -> Option<arreio_eqsat::Expr> {
        match expr {
            Expr::Const(c) => Some(arreio_eqsat::Expr::Const(*c)),
            Expr::Var(v) => Some(arreio_eqsat::Expr::Var(v.clone())),
            Expr::Add(a, b) => Some(arreio_eqsat::Expr::Add(
                Box::new(Self::to_eqsat_expr(a)?),
                Box::new(Self::to_eqsat_expr(b)?),
            )),
            Expr::Mul(a, b) => Some(arreio_eqsat::Expr::Mul(
                Box::new(Self::to_eqsat_expr(a)?),
                Box::new(Self::to_eqsat_expr(b)?),
            )),
            _ => None,
        }
    }

    /// Converte Expr do eqsat de volta para Expr do supercompile.
    fn from_eqsat_expr(expr: &arreio_eqsat::Expr) -> Expr {
        match expr {
            arreio_eqsat::Expr::Const(c) => Expr::Const(*c),
            arreio_eqsat::Expr::Var(v) => Expr::Var(v.clone()),
            arreio_eqsat::Expr::Add(a, b) => Expr::Add(
                Box::new(Self::from_eqsat_expr(a)),
                Box::new(Self::from_eqsat_expr(b)),
            ),
            arreio_eqsat::Expr::Mul(a, b) => Expr::Mul(
                Box::new(Self::from_eqsat_expr(a)),
                Box::new(Self::from_eqsat_expr(b)),
            ),
            arreio_eqsat::Expr::Neg(a) => {
                // Representa -a como 0 - a via Mul(-1, a) já que supercompile não tem Neg
                Expr::Mul(
                    Box::new(Expr::Const(-1)),
                    Box::new(Self::from_eqsat_expr(a)),
                )
            }
        }
    }

    /// Regras de normalização pós-eqsat para manter compatibilidade:
    /// - Commutativity: normaliza Const à direita, Var à esquerda
    /// - Identity folding: x + 0 => x, x * 1 => x
    fn eqsat_normalize_rules(expr: &Expr, changed: &mut bool) -> Expr {
        match expr {
            // Commutativity: normaliza Const à direita, Var à esquerda
            Expr::Add(a, b)
                if matches!(a.as_ref(), Expr::Const(_)) && matches!(b.as_ref(), Expr::Var(_)) =>
            {
                *changed = true;
                Expr::Add(b.clone(), a.clone())
            }
            Expr::Mul(a, b)
                if matches!(a.as_ref(), Expr::Const(_)) && matches!(b.as_ref(), Expr::Var(_)) =>
            {
                *changed = true;
                Expr::Mul(b.clone(), a.clone())
            }
            // Identity folding + recursão
            Expr::Add(a, b) => {
                let na = Self::eqsat_normalize_rules(a, changed);
                let nb = Self::eqsat_normalize_rules(b, changed);
                if let Expr::Const(0) = nb {
                    *changed = true;
                    na
                } else {
                    Expr::Add(Box::new(na), Box::new(nb))
                }
            }
            Expr::Mul(a, b) => {
                let na = Self::eqsat_normalize_rules(a, changed);
                let nb = Self::eqsat_normalize_rules(b, changed);
                if let Expr::Const(1) = nb {
                    *changed = true;
                    na
                } else if let Expr::Const(1) = na {
                    *changed = true;
                    nb
                } else {
                    Expr::Mul(Box::new(na), Box::new(nb))
                }
            }
            Expr::If(c, t, e) => Expr::If(
                Box::new(Self::eqsat_normalize_rules(c, changed)),
                Box::new(Self::eqsat_normalize_rules(t, changed)),
                Box::new(Self::eqsat_normalize_rules(e, changed)),
            ),
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(Self::eqsat_normalize_rules(val, changed)),
                Box::new(Self::eqsat_normalize_rules(body, changed)),
            ),
            Expr::Call(name, args) => Expr::Call(
                name.clone(),
                args.iter()
                    .map(|a| Self::eqsat_normalize_rules(a, changed))
                    .collect(),
            ),
            _ => expr.clone(),
        }
    }

    /// Calcula percentual de redução de tamanho.
    pub fn size_reduction(&self, original: usize, optimized: usize) -> f64 {
        if original == 0 {
            0.0
        } else {
            ((original.saturating_sub(optimized)) as f64 / original as f64) * 100.0
        }
    }

    /// Gera relatório human-readable das otimizações aplicadas.
    pub fn optimizations_report(&self, output: &OptimizedOutput) -> String {
        let reduction = self.size_reduction(output.original_size, output.optimized_size);
        let mut report = format!(
            "=== Relatório de Otimização ===\n\
             Tamanho original: {}\n\
             Tamanho otimizado: {}\n\
             Redução: {:.2}%\n\
             Verificação: {}\n\
             Otimizações aplicadas ({}):\n",
            output.original_size,
            output.optimized_size,
            reduction,
            if output.verification_passed {
                "PASSOU"
            } else {
                "FALHOU"
            },
            output.optimizations_applied.len()
        );
        for opt in &output.optimizations_applied {
            report.push_str(&format!("  - {}\n", opt));
        }
        report
    }
}

impl Default for PostGenPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Expr, Function};

    #[test]
    fn pipeline_processes_simple_code() {
        let program = Program {
            functions: vec![Function {
                name: "inc".to_string(),
                params: vec!["n".to_string()],
                body: Expr::Add(
                    Box::new(Expr::Var("n".to_string())),
                    Box::new(Expr::Const(1)),
                ),
            }],
            entry: Expr::Call("inc".to_string(), vec![Expr::Const(5)]),
        };
        let json = serde_json::to_string(&program).unwrap();
        let pipeline = PostGenPipeline::new();
        let output = pipeline.process(&json).unwrap();
        // Verificação formal é responsabilidade do caller (ContractEngine/HoareTriple).
        // O pipeline de otimização não retorna placebo — verification_passed é false.
        assert!(!output.verification_passed);
        assert!(!output.optimizations_applied.is_empty());
    }

    #[test]
    fn slicing_removes_dead_code() {
        let program = Program {
            functions: vec![
                Function {
                    name: "used".to_string(),
                    params: vec!["x".to_string()],
                    body: Expr::Var("x".to_string()),
                },
                Function {
                    name: "dead".to_string(),
                    params: vec!["y".to_string()],
                    body: Expr::Const(99),
                },
            ],
            entry: Expr::Call("used".to_string(), vec![Expr::Const(1)]),
        };
        let json = serde_json::to_string(&program).unwrap();
        let pipeline = PostGenPipeline::new();
        let output = pipeline.process(&json).unwrap();
        let optimized: Program = serde_json::from_str(&output.code).unwrap();
        assert_eq!(optimized.functions.len(), 1);
        assert_eq!(optimized.functions[0].name, "used");
    }

    #[test]
    fn slicing_removes_unused_let_bindings() {
        let program = Program {
            functions: vec![],
            entry: Expr::Let(
                "unused".to_string(),
                Box::new(Expr::Const(42)),
                Box::new(Expr::Const(7)),
            ),
        };
        let json = serde_json::to_string(&program).unwrap();
        let pipeline = PostGenPipeline::new();
        let output = pipeline.process(&json).unwrap();
        let optimized: Program = serde_json::from_str(&output.code).unwrap();
        assert_eq!(optimized.entry, Expr::Const(7));
    }

    #[test]
    fn supercompilation_reduces_expressions() {
        let program = Program {
            functions: vec![],
            entry: Expr::Add(Box::new(Expr::Const(2)), Box::new(Expr::Const(3))),
        };
        let json = serde_json::to_string(&program).unwrap();
        let pipeline = PostGenPipeline::new();
        let output = pipeline.process(&json).unwrap();
        assert!(output.optimized_size < output.original_size);
    }

    #[test]
    fn metrics_calculate_reduction_correctly() {
        let pipeline = PostGenPipeline::new();
        assert!((pipeline.size_reduction(100, 75) - 25.0).abs() < f64::EPSILON);
        assert!((pipeline.size_reduction(200, 50) - 75.0).abs() < f64::EPSILON);
        assert_eq!(pipeline.size_reduction(0, 0), 0.0);
    }

    #[test]
    fn report_lists_optimizations() {
        let output = OptimizedOutput {
            code: "{}".to_string(),
            optimizations_applied: vec![
                "program_slicing".to_string(),
                "supercompilation".to_string(),
            ],
            verification_passed: true,
            original_size: 100,
            optimized_size: 80,
        };
        let pipeline = PostGenPipeline::new();
        let report = pipeline.optimizations_report(&output);
        assert!(report.contains("program_slicing"));
        assert!(report.contains("supercompilation"));
        assert!(report.contains("PASSOU"));
        assert!(report.contains("20.00%"));
    }

    #[test]
    fn all_disabled_returns_original() {
        let program = Program {
            functions: vec![],
            entry: Expr::Add(
                Box::new(Expr::Var("x".to_string())),
                Box::new(Expr::Var("y".to_string())),
            ),
        };
        let json = serde_json::to_string(&program).unwrap();
        let pipeline = PostGenPipeline {
            supercompile: SupercompilePipeline,
            supercompile_enabled: false,
            eqsat_enabled: false,
            slice_enabled: false,
        };
        let output = pipeline.process(&json).unwrap();
        assert_eq!(output.original_size, output.optimized_size);
        assert!(output.optimizations_applied.is_empty());
    }
}
