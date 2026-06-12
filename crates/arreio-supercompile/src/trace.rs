use crate::program::{Expr, Function, Program};
use std::collections::HashMap;

/// Passo de execução em um trace.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceStep {
    pub function: String,
    pub args: Vec<i64>,
    pub result: i64,
}

/// Trace de execução completo.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionTrace {
    pub steps: Vec<TraceStep>,
}

/// Caminho quente identificado no trace.
#[derive(Debug, Clone, PartialEq)]
pub struct HotPath {
    pub sequence: Vec<String>,
    pub frequency: usize,
    pub optimized_expr: Expr,
}

/// Otimização baseada em traces de execução.
pub struct TraceOptimizer {
    traces: Vec<ExecutionTrace>,
}

impl TraceOptimizer {
    pub fn new() -> Self {
        Self { traces: Vec::new() }
    }

    /// Registra um trace de execução.
    pub fn record_trace(&mut self, trace: ExecutionTrace) {
        self.traces.push(trace);
    }

    /// Identifica hot paths no trace (sequências frequentes de chamadas).
    pub fn identify_hot_paths(&self) -> Vec<HotPath> {
        let mut seq_counts: HashMap<Vec<String>, usize> = HashMap::new();

        for trace in &self.traces {
            let names: Vec<String> = trace.steps.iter().map(|s| s.function.clone()).collect();

            // Conta subsequências de tamanho 2 a 5
            for len in 2..=5 {
                for window in names.windows(len) {
                    let seq = window.to_vec();
                    *seq_counts.entry(seq).or_insert(0) += 1;
                }
            }
        }

        let mut hot_paths: Vec<HotPath> = seq_counts
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|(seq, count)| HotPath {
                sequence: seq.clone(),
                frequency: count,
                optimized_expr: Expr::Call(format!("__hot_{}", seq.join("_")), Vec::new()),
            })
            .collect();

        // Ordena por frequência decrescente
        hot_paths.sort_by(|a, b| b.frequency.cmp(&a.frequency));
        hot_paths
    }

    /// Otimiza programa com base em hot paths.
    pub fn optimize_hot_paths(&self, program: &Program) -> Program {
        let hot_paths = self.identify_hot_paths();
        if hot_paths.is_empty() {
            return program.clone();
        }

        let mut functions = program.functions.clone();
        let mut entry = program.entry.clone();

        // Substitui chamadas diretas sequenciais por chamadas ao hot path
        for hp in &hot_paths {
            let hot_fn_name = format!("__hot_{}", hp.sequence.join("_"));

            // Cria função hot path que encapsula a sequência
            let hot_body = self.build_hot_path_body(&hp.sequence, program);
            functions.push(Function {
                name: hot_fn_name.clone(),
                params: Vec::new(),
                body: hot_body,
            });

            entry = self.replace_sequence_in_expr(&entry, &hp.sequence, &hot_fn_name);
            for f in &mut functions {
                f.body = self.replace_sequence_in_expr(&f.body, &hp.sequence, &hot_fn_name);
            }
        }

        Program { functions, entry }
    }

    fn build_hot_path_body(&self, sequence: &[String], _program: &Program) -> Expr {
        // Constrói uma cadeia de chamadas simplificada representando o hot path
        let mut body = Expr::Const(0);
        for name in sequence.iter().rev() {
            body = Expr::Call(name.clone(), vec![body]);
        }
        body
    }

    fn replace_sequence_in_expr(
        &self,
        expr: &Expr,
        sequence: &[String],
        hot_fn_name: &str,
    ) -> Expr {
        // Substituição simplificada: procura Call(name, ...) onde name está na sequência
        match expr {
            Expr::Var(_) | Expr::Const(_) => expr.clone(),
            Expr::Add(a, b) => Expr::Add(
                Box::new(self.replace_sequence_in_expr(a, sequence, hot_fn_name)),
                Box::new(self.replace_sequence_in_expr(b, sequence, hot_fn_name)),
            ),
            Expr::Mul(a, b) => Expr::Mul(
                Box::new(self.replace_sequence_in_expr(a, sequence, hot_fn_name)),
                Box::new(self.replace_sequence_in_expr(b, sequence, hot_fn_name)),
            ),
            Expr::If(c, t, e) => Expr::If(
                Box::new(self.replace_sequence_in_expr(c, sequence, hot_fn_name)),
                Box::new(self.replace_sequence_in_expr(t, sequence, hot_fn_name)),
                Box::new(self.replace_sequence_in_expr(e, sequence, hot_fn_name)),
            ),
            Expr::Let(name, val, body) => Expr::Let(
                name.clone(),
                Box::new(self.replace_sequence_in_expr(val, sequence, hot_fn_name)),
                Box::new(self.replace_sequence_in_expr(body, sequence, hot_fn_name)),
            ),
            Expr::Call(name, args) => {
                let new_args: Vec<Expr> = args
                    .iter()
                    .map(|a| self.replace_sequence_in_expr(a, sequence, hot_fn_name))
                    .collect();

                if sequence.len() == 1 && sequence[0] == *name {
                    Expr::Call(hot_fn_name.to_string(), new_args)
                } else {
                    Expr::Call(name.clone(), new_args)
                }
            }
        }
    }
}

impl Default for TraceOptimizer {
    fn default() -> Self {
        Self::new()
    }
}
