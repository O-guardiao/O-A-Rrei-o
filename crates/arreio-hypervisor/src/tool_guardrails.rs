use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Estado do guardrail por sessão de execução.
pub struct ToolGuardrailController {
    /// Contagem de falhas por (tool_name, hash_args).
    failure_counts: HashMap<(String, u64), u32>,
    /// Hash do resultado da última execução idempotente.
    last_idempotent_result: HashMap<String, u64>,
    /// Limite para warn.
    warn_after: u32,
    /// Limite para hard stop.
    hard_stop_after: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardrailDecision {
    Allow,
    Warn,
    Block,
    Halt,
}

impl ToolGuardrailController {
    pub fn new(warn_after: u32, hard_stop_after: u32) -> Self {
        Self {
            failure_counts: HashMap::new(),
            last_idempotent_result: HashMap::new(),
            warn_after,
            hard_stop_after,
        }
    }

    /// Avalia uma execução de tool antes de prosseguir.
    pub fn evaluate(
        &mut self,
        tool_name: &str,
        args: &str,
        is_idempotent: bool,
    ) -> GuardrailDecision {
        let args_hash = hash_of(args);
        let key = (tool_name.to_string(), args_hash);

        // Se já atingiu hard stop, mantém halt
        if let Some(&count) = self.failure_counts.get(&key) {
            if count >= self.hard_stop_after {
                return GuardrailDecision::Halt;
            }
            if count >= self.warn_after {
                return GuardrailDecision::Warn;
            }
        }

        // Para tools idempotentes: detecta no-progress (mesmo resultado)
        if is_idempotent {
            // O resultado será verificado após execução via record_result
            return GuardrailDecision::Allow;
        }

        GuardrailDecision::Allow
    }

    /// Registra o resultado após execução.
    pub fn record_result(&mut self, tool_name: &str, args: &str, success: bool, result: &str) {
        let args_hash = hash_of(args);
        let key = (tool_name.to_string(), args_hash);

        if success {
            self.failure_counts.remove(&key);
        } else {
            *self.failure_counts.entry(key).or_insert(0) += 1;
        }

        // Detecta no-progress em idempotent
        if is_tool_idempotent(tool_name) {
            let result_hash = hash_of(result);
            if let Some(last) = self.last_idempotent_result.get(tool_name) {
                if *last == result_hash && !success {
                    // Mesmo resultado falho — possível loop
                    let loop_key = (tool_name.to_string(), args_hash);
                    *self.failure_counts.entry(loop_key).or_insert(0) += 1;
                }
            }
            self.last_idempotent_result
                .insert(tool_name.to_string(), result_hash);
        }
    }

    pub fn reset(&mut self) {
        self.failure_counts.clear();
        self.last_idempotent_result.clear();
    }
}

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn is_tool_idempotent(name: &str) -> bool {
    let idempotent = [
        "read", "cat", "grep", "find", "ls", "status", "search", "get",
    ];
    idempotent.iter().any(|i| name.to_lowercase().contains(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permite_primeira_execucao() {
        let mut ctrl = ToolGuardrailController::new(2, 3);
        assert_eq!(ctrl.evaluate("read", "{}", true), GuardrailDecision::Allow);
    }

    #[test]
    fn warn_apos_falhas_repetidas() {
        let mut ctrl = ToolGuardrailController::new(2, 5);
        ctrl.record_result("cmd", "ls", false, "err");
        ctrl.record_result("cmd", "ls", false, "err");
        assert_eq!(ctrl.evaluate("cmd", "ls", false), GuardrailDecision::Warn);
    }

    #[test]
    fn halt_apos_hard_stop() {
        let mut ctrl = ToolGuardrailController::new(1, 2);
        ctrl.record_result("cmd", "ls", false, "err");
        ctrl.record_result("cmd", "ls", false, "err");
        assert_eq!(ctrl.evaluate("cmd", "ls", false), GuardrailDecision::Halt);
    }
}
