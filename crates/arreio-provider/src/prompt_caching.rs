use serde::{Deserialize, Serialize};

/// Estratégia de cache control para Anthropic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStrategy {
    /// 4 breakpoints: system + 3 primeiros turns.
    SystemAnd3,
    /// Sem cache.
    None,
}

/// Breakpoint de cache_control em uma mensagem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub control_type: String, // "ephemeral"
}

/// Adiciona cache_control breakpoints a mensagens.
pub struct PromptCaching;

impl PromptCaching {
    /// Aplica a estratégia de cache a um vetor de mensagens.
    /// Retorna as mensagens com cache_control inserido nos breakpoints.
    pub fn apply(
        strategy: CacheStrategy,
        system_prompt: Option<&str>,
        messages: &mut Vec<serde_json::Value>,
    ) {
        if strategy == CacheStrategy::None {
            return;
        }

        // Estratégia SystemAnd3: marca system prompt + 3 primeiras mensagens
        if let Some(_system) = system_prompt {
            // System prompt é tratado separadamente na API Anthropic
            // Aqui apenas marcamos as mensagens
        }

        let breakpoints = match strategy {
            CacheStrategy::SystemAnd3 => 4,
            CacheStrategy::None => 0,
        };

        for (i, msg) in messages.iter_mut().enumerate() {
            if i < breakpoints {
                if let Some(obj) = msg.as_object_mut() {
                    obj.insert(
                        "cache_control".to_string(),
                        serde_json::to_value(CacheControl {
                            control_type: "ephemeral".to_string(),
                        })
                        .unwrap(),
                    );
                }
            }
        }
    }

    /// Estima redução de tokens (~75% em multi-turn).
    pub fn estimate_savings(messages_count: usize, _strategy: CacheStrategy) -> f64 {
        if messages_count <= 1 {
            return 0.0;
        }
        // Aproximação: 75% de redução nos tokens de input após o primeiro turno
        let cached_turns = (messages_count.saturating_sub(1)) as f64;
        let total_turns = messages_count as f64;
        (cached_turns / total_turns) * 0.75
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_system_and_3() {
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({"role": "assistant", "content": "hi"}),
            serde_json::json!({"role": "user", "content": "again"}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "user", "content": "last"}),
        ];

        PromptCaching::apply(CacheStrategy::SystemAnd3, Some("system"), &mut messages);

        // Primeiras 4 mensagens devem ter cache_control
        for i in 0..4 {
            assert!(
                messages[i].get("cache_control").is_some(),
                "msg {} deve ter cache_control",
                i
            );
        }
        // Última não
        assert!(messages[4].get("cache_control").is_none());
    }

    #[test]
    fn none_strategy_noop() {
        let mut messages = vec![serde_json::json!({"role": "user", "content": "hello"})];
        PromptCaching::apply(CacheStrategy::None, None, &mut messages);
        assert!(messages[0].get("cache_control").is_none());
    }

    #[test]
    fn estimate_savings_multi_turn() {
        let savings = PromptCaching::estimate_savings(5, CacheStrategy::SystemAnd3);
        assert!(savings > 0.5 && savings < 0.8);
    }

    #[test]
    fn estimate_no_savings_single_turn() {
        let savings = PromptCaching::estimate_savings(1, CacheStrategy::SystemAnd3);
        assert_eq!(savings, 0.0);
    }
}
