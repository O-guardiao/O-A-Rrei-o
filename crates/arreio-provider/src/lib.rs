pub mod anthropic;
pub mod azure;
pub mod circuit;
pub mod cost_tracker;
pub mod deepseek;
pub mod error_classifier;
pub mod frozen_snapshot;
pub mod google;
pub mod llm_hooks;
pub mod mock;
pub mod ollama;
pub mod openai;
pub mod pool;
pub mod prompt_caching;
pub mod prompt_mode;
pub mod provider;
pub mod rate_guard;
pub mod request_classifier;
pub mod routing_policy;
pub mod recovery_block;
pub mod tls_client;

pub use anthropic::AnthropicProvider;
pub use azure::AzureProvider;
pub use circuit::{CircuitBreaker, CircuitError, CircuitState};
pub use cost_tracker::{CostReport, CostTracker, SessionCost};
pub use deepseek::DeepseekProvider;
pub use error_classifier::{ClassifiedError, ErrorClassifier, FailoverReason, RecoveryAction};
pub use frozen_snapshot::{
    CacheStats, FrozenSnapshot, SnapshotCache, SystemPromptBuilder, SystemPromptLayers,
};
pub use google::GoogleProvider;
pub use llm_hooks::{HookedProvider, PostLlmHook, PreLlmHook};
pub use mock::MockProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAiCompatProvider;
pub use pool::{FailoverStrategy, ProviderPool, RoutingStrategy};
pub use request_classifier::{
    ClassifiedRequest, RequestClassifier, RequestType, SensitivityLevel, TaskComplexity,
};
pub use routing_policy::{BudgetStatus, RoutingDecision, RoutingPolicy};
pub use prompt_caching::{CacheControl, CacheStrategy, PromptCaching};
pub use prompt_mode::PromptMode;
pub use provider::{
    ChatMessageRequest, ChatRequest, ChatResponse, ProviderClient, ToolCall, ToolCallFunction,
    ToolDescriptor, ToolFunction,
};
pub use rate_guard::{RateLimitError, RateLimitGuard, RateLimitSnapshot, RateLimitState};
pub use recovery_block::{
    AcceptanceResult, AcceptanceTest, CompositeTest, ContentPatternTest, FormatValidationTest,
    GitStateRestorer, NoOpStateRestorer, RecoveryBlockManager, RecoveryBlockResult, StateRestorer,
    TokenBoundsTest,
};
pub use tls_client::TlsClient;

/// Faz parsing de uma resposta HTTP/1.x bruta em (status, headers, body).
pub(crate) fn parse_http_response(
    raw: &str,
) -> anyhow::Result<(u16, std::collections::HashMap<String, String>, String)> {
    use anyhow::Context;
    let mut lines = raw.lines();
    let status_line = lines.next().context("resposta HTTP vazia")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .context("status HTTP inválido")?;

    let mut headers = std::collections::HashMap::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((status, headers, body))
}
