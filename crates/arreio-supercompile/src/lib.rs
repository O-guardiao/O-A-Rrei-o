pub mod eqsat_bridge;
pub mod partial_eval;
pub mod pipeline;
pub mod program;
pub mod supercompiler;
pub mod trace;

use crate::partial_eval::PartialEvaluator;
use crate::pipeline::{OptimizedOutput, PostGenPipeline};
use crate::program::Program;
use crate::supercompiler::Supercompiler;
use anyhow::Result;
use std::collections::HashMap;

/// API de alto nível chamada pelo Developer actor após gerar código.
pub fn optimize_code(input: &str) -> Result<OptimizedOutput> {
    let pipeline = PostGenPipeline::new();
    pipeline.process(input)
}

/// Pipeline completo de supercompilação.
pub struct SupercompilePipeline;

impl SupercompilePipeline {
    pub fn run(program: &Program, known_inputs: &HashMap<String, i64>) -> Program {
        // 1. Partial evaluation
        let pe = PartialEvaluator::new();
        let mut entry = pe.evaluate(&program.entry, known_inputs);
        let mut functions: Vec<_> = program
            .functions
            .iter()
            .map(|f| crate::program::Function {
                name: f.name.clone(),
                params: f.params.clone(),
                body: pe.evaluate(&f.body, known_inputs),
            })
            .collect();

        let partial_program = Program {
            functions: functions.clone(),
            entry: entry.clone(),
        };

        // Inline de funções pequenas
        let inlined = pe.inline_functions(&partial_program, 10);
        entry = inlined.entry;
        functions = inlined.functions;

        // 2. Supercompilation (driving + folding)
        let mut sc = Supercompiler::new();
        let supercompiled = sc.supercompile(
            &Program {
                functions: functions.clone(),
                entry: entry.clone(),
            },
            known_inputs,
        );
        entry = supercompiled.entry;
        functions = supercompiled.functions;

        // 3. Equality Saturation via arreio-eqsat (otimização por e-graphs).
        // Aplica a entry e a cada função que seja conversível.
        if let Some(opt_entry) = eqsat_bridge::optimize_expr(&entry) {
            entry = opt_entry;
        }
        for f in &mut functions {
            if let Some(opt_body) = eqsat_bridge::optimize_expr(&f.body) {
                f.body = opt_body;
            }
        }

        // 4. Dead code elimination
        entry = pe.dead_code_elimination(&entry);
        for f in &mut functions {
            f.body = pe.dead_code_elimination(&f.body);
        }

        Program { functions, entry }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Expr, Function, Program};
    use crate::trace::{ExecutionTrace, TraceOptimizer, TraceStep};
    use std::collections::HashMap;

    #[test]
    fn constant_propagation_basic() {
        let pe = PartialEvaluator::new();
        // (2 + 3) * x  =>  5 * x
        let expr = Expr::Mul(
            Box::new(Expr::Add(
                Box::new(Expr::Const(2)),
                Box::new(Expr::Const(3)),
            )),
            Box::new(Expr::Var("x".to_string())),
        );
        let result = pe.constant_propagation(&expr);
        assert_eq!(
            result,
            Expr::Mul(
                Box::new(Expr::Const(5)),
                Box::new(Expr::Var("x".to_string()))
            )
        );
    }

    #[test]
    fn dead_code_elimination_if() {
        let pe = PartialEvaluator::new();
        // if (1) then 42 else 99  =>  42
        let expr = Expr::If(
            Box::new(Expr::Const(1)),
            Box::new(Expr::Const(42)),
            Box::new(Expr::Const(99)),
        );
        let result = pe.dead_code_elimination(&expr);
        assert_eq!(result, Expr::Const(42));

        // if (0) then 42 else 99  =>  99
        let expr2 = Expr::If(
            Box::new(Expr::Const(0)),
            Box::new(Expr::Const(42)),
            Box::new(Expr::Const(99)),
        );
        let result2 = pe.dead_code_elimination(&expr2);
        assert_eq!(result2, Expr::Const(99));
    }

    #[test]
    fn partial_eval_with_known_inputs() {
        let pe = PartialEvaluator::new();
        let mut env = HashMap::new();
        env.insert("x".to_string(), 10);
        env.insert("y".to_string(), 5);

        // x + y + z  =>  15 + z
        let expr = Expr::Add(
            Box::new(Expr::Add(
                Box::new(Expr::Var("x".to_string())),
                Box::new(Expr::Var("y".to_string())),
            )),
            Box::new(Expr::Var("z".to_string())),
        );
        let result = pe.evaluate(&expr, &env);
        assert_eq!(
            result,
            Expr::Add(
                Box::new(Expr::Const(15)),
                Box::new(Expr::Var("z".to_string()))
            )
        );
    }

    #[test]
    fn supercompile_simple_program() {
        let program = Program {
            functions: vec![Function {
                name: "double".to_string(),
                params: vec!["a".to_string()],
                body: Expr::Add(
                    Box::new(Expr::Var("a".to_string())),
                    Box::new(Expr::Var("a".to_string())),
                ),
            }],
            entry: Expr::Call("double".to_string(), vec![Expr::Const(7)]),
        };

        let mut sc = Supercompiler::new();
        let known = HashMap::new();
        let result = sc.supercompile(&program, &known);

        // Entry deve ser especializada (call permanece pois não há inlining no supercompiler)
        assert_eq!(
            result.entry,
            Expr::Call("double".to_string(), vec![Expr::Const(7)])
        );
        // A função double deve existir
        assert_eq!(result.functions.len(), 1);
    }

    #[test]
    fn drive_reduces_expr() {
        let mut sc = Supercompiler::new();
        // let a = 3 in a + 5  =>  8
        let expr = Expr::Let(
            "a".to_string(),
            Box::new(Expr::Const(3)),
            Box::new(Expr::Add(
                Box::new(Expr::Var("a".to_string())),
                Box::new(Expr::Const(5)),
            )),
        );
        let result = sc.drive(&expr);
        assert_eq!(result, Expr::Const(8));
    }

    #[test]
    fn fold_detects_recursion() {
        let mut sc = Supercompiler::new();
        // Expressões similares em histórico devem disparar folding
        let e1 = Expr::Add(
            Box::new(Expr::Const(1)),
            Box::new(Expr::Var("x".to_string())),
        );
        let e2 = Expr::Add(
            Box::new(Expr::Const(2)),
            Box::new(Expr::Var("x".to_string())),
        );
        let history = vec![e1.clone()];
        let result = sc.fold(&e2, &history);

        // Deve retornar uma Call para função gerada
        assert!(matches!(result, Expr::Call(_, _)));
        // Deve ter criado uma nova função
        assert!(!sc.generated_functions().is_empty());
    }

    #[test]
    fn generalize_similar_exprs() {
        let sc = Supercompiler::new();
        let e1 = Expr::Add(
            Box::new(Expr::Const(1)),
            Box::new(Expr::Var("x".to_string())),
        );
        let e2 = Expr::Add(
            Box::new(Expr::Const(2)),
            Box::new(Expr::Var("x".to_string())),
        );
        let gen = sc.generalize(&e1, &e2);
        // Constantes diferentes generalizam para variável __g
        assert_eq!(
            gen,
            Expr::Add(
                Box::new(Expr::Var("__g".to_string())),
                Box::new(Expr::Var("x".to_string()))
            )
        );
    }

    #[test]
    fn residualize_generates_program() {
        let sc = Supercompiler::new();
        // (3 + 4) * x  =>  7 * x
        let expr = Expr::Mul(
            Box::new(Expr::Add(
                Box::new(Expr::Const(3)),
                Box::new(Expr::Const(4)),
            )),
            Box::new(Expr::Var("x".to_string())),
        );
        let result = sc.residualize(&expr);
        assert_eq!(
            result,
            Expr::Mul(
                Box::new(Expr::Const(7)),
                Box::new(Expr::Var("x".to_string()))
            )
        );
    }

    #[test]
    fn trace_hot_path_identification() {
        let mut to = TraceOptimizer::new();
        let trace1 = ExecutionTrace {
            steps: vec![
                TraceStep {
                    function: "f".to_string(),
                    args: vec![1],
                    result: 2,
                },
                TraceStep {
                    function: "g".to_string(),
                    args: vec![2],
                    result: 3,
                },
                TraceStep {
                    function: "h".to_string(),
                    args: vec![3],
                    result: 4,
                },
            ],
        };
        let trace2 = ExecutionTrace {
            steps: vec![
                TraceStep {
                    function: "f".to_string(),
                    args: vec![2],
                    result: 3,
                },
                TraceStep {
                    function: "g".to_string(),
                    args: vec![3],
                    result: 4,
                },
                TraceStep {
                    function: "h".to_string(),
                    args: vec![4],
                    result: 5,
                },
            ],
        };
        to.record_trace(trace1);
        to.record_trace(trace2);

        let hot_paths = to.identify_hot_paths();
        assert!(!hot_paths.is_empty());

        // Sequência ["f", "g"] deve aparecer com frequência 2
        let fg = hot_paths.iter().find(|hp| hp.sequence == vec!["f", "g"]);
        assert!(fg.is_some());
        assert_eq!(fg.unwrap().frequency, 2);
    }

    #[test]
    fn pipeline_full_optimization() {
        let program = Program {
            functions: vec![
                Function {
                    name: "inc".to_string(),
                    params: vec!["n".to_string()],
                    body: Expr::Add(
                        Box::new(Expr::Var("n".to_string())),
                        Box::new(Expr::Const(1)),
                    ),
                },
                Function {
                    name: "add3".to_string(),
                    params: vec!["a".to_string()],
                    body: Expr::Call(
                        "inc".to_string(),
                        vec![Expr::Call(
                            "inc".to_string(),
                            vec![Expr::Call(
                                "inc".to_string(),
                                vec![Expr::Var("a".to_string())],
                            )],
                        )],
                    ),
                },
            ],
            entry: Expr::Call("add3".to_string(), vec![Expr::Const(5)]),
        };

        let known = HashMap::new();
        let optimized = SupercompilePipeline::run(&program, &known);

        // O programa otimizado deve conter a entry
        assert!(!optimized.functions.is_empty());
    }
}
