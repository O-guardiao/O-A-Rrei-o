//! Error Classification Pipeline — FASE 9.2
//!
//! Classifica erros de providers LLM em `FailoverReason` e determina a
//! `RecoveryAction` apropriada, permitindo decisões inteligentes de retry,
//! compressão, fallback ou abort.

use std::fmt;

/// Taxonomia de razões de failover entre providers LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverReason {
    /// Falha de autenticação que pode ser resolvida com retry (token expirado).
    Auth,
    /// Falha de autenticação permanente (API key inválida, revogada).
    AuthPermanent,
    /// Limite de faturamento atingido ou quota esgotada.
    Billing,
    /// Rate limit atingido (throttling).
    RateLimit,
    /// Provider sobrecarregado (capacity exceeded).
    Overloaded,
    /// Erro interno do servidor do provider.
    ServerError,
    /// Timeout de conexão ou resposta.
    Timeout,
    /// Contexto da sessão excede o limite do modelo.
    ContextOverflow,
    /// Payload da requisição excede limites.
    PayloadTooLarge,
    /// Imagem ou arquivo anexo muito grande.
    ImageTooLarge,
    /// Modelo solicitado não encontrado.
    ModelNotFound,
    /// Conteúdo bloqueado por política do provider.
    ProviderPolicyBlocked,
    /// Erro de formato, schema JSON, grammar ou tool descriptor.
    FormatError,
    /// Assinatura de thinking/reasoning inválida ou não suportada.
    ThinkingSignature,
    /// Requisição exige tier de contexto longo não disponível.
    LongContextTier,
}

impl fmt::Display for FailoverReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FailoverReason::Auth => "Auth",
            FailoverReason::AuthPermanent => "AuthPermanent",
            FailoverReason::Billing => "Billing",
            FailoverReason::RateLimit => "RateLimit",
            FailoverReason::Overloaded => "Overloaded",
            FailoverReason::ServerError => "ServerError",
            FailoverReason::Timeout => "Timeout",
            FailoverReason::ContextOverflow => "ContextOverflow",
            FailoverReason::PayloadTooLarge => "PayloadTooLarge",
            FailoverReason::ImageTooLarge => "ImageTooLarge",
            FailoverReason::ModelNotFound => "ModelNotFound",
            FailoverReason::ProviderPolicyBlocked => "ProviderPolicyBlocked",
            FailoverReason::FormatError => "FormatError",
            FailoverReason::ThinkingSignature => "ThinkingSignature",
            FailoverReason::LongContextTier => "LongContextTier",
        };
        write!(f, "{}", s)
    }
}

/// Ação de recuperação recomendada para um erro classificado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry simples com backoff é apropriado.
    Retryable,
    /// Comprimir contexto e tentar novamente no mesmo provider.
    ShouldCompress,
    /// Rotacionar credencial / tentar próximo provider.
    ShouldRotateCredential,
    /// Ativar cadeia de fallback para outro provider.
    ShouldFallback,
    /// Abortar imediatamente — não há recuperação possível.
    Abort,
}

impl fmt::Display for RecoveryAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            RecoveryAction::Retryable => "Retryable",
            RecoveryAction::ShouldCompress => "ShouldCompress",
            RecoveryAction::ShouldRotateCredential => "ShouldRotateCredential",
            RecoveryAction::ShouldFallback => "ShouldFallback",
            RecoveryAction::Abort => "Abort",
        };
        write!(f, "{}", s)
    }
}

/// Erro classificado com razão, ação e descrição legível.
#[derive(Debug, Clone)]
pub struct ClassifiedError {
    pub reason: FailoverReason,
    pub action: RecoveryAction,
    pub description: String,
}

/// Classificador de erros de providers LLM.
///
/// Implementa pipeline prioritária de classificação baseada em:
/// 1. Padrões específicos de provider (thinking sig, tier gates, grammar).
/// 2. Código HTTP + refinamento por mensagem.
/// 3. Código de erro extraído do corpo da resposta.
/// 4. Matching por padrão na mensagem.
/// 5. SSL/TLS transient → timeout.
/// 6. Server disconnect + sessão grande → context_overflow.
/// 7. Fallback: unknown (retryable).
pub struct ErrorClassifier;

impl ErrorClassifier {
    /// Classifica um erro `anyhow` sem informações de contexto adicionais.
    pub fn classify(error: &anyhow::Error) -> ClassifiedError {
        Self::classify_with_context(error, 0, 0)
    }

    /// Classifica um erro considerando tamanho da sessão.
    ///
    /// `tokens_in`: tokens estimados da requisição atual.
    /// `max_context`: limite de contexto do modelo (0 = desconhecido).
    pub fn classify_with_context(
        error: &anyhow::Error,
        tokens_in: u64,
        max_context: u64,
    ) -> ClassifiedError {
        let msg = error.to_string().to_lowercase();
        let large_session = max_context > 0 && tokens_in > max_context / 2;

        // Pipeline prioritária
        if let Some(c) = Self::provider_specific_patterns(&msg) {
            return c;
        }
        if let Some(c) = Self::http_status_refinement(&msg) {
            return c;
        }
        if let Some(c) = Self::error_code_classification(&msg) {
            return c;
        }
        if let Some(c) = Self::message_pattern_matching(&msg) {
            return c;
        }
        if let Some(c) = Self::ssl_tls_transient(&msg) {
            return c;
        }
        if let Some(c) = Self::server_disconnect_context(&msg, large_session) {
            return c;
        }

        // Fallback: unknown → retryable
        ClassifiedError {
            reason: FailoverReason::ServerError,
            action: RecoveryAction::Retryable,
            description: format!("Erro não classificado — tratado como retryable: {}", msg),
        }
    }

    // ------------------------------------------------------------------
    // Etapas do pipeline
    // ------------------------------------------------------------------

    fn provider_specific_patterns(msg: &str) -> Option<ClassifiedError> {
        if msg.contains("thinking signature")
            || msg.contains("thinking_signature")
            || msg.contains("invalid thinking")
            || msg.contains("reasoning signature")
        {
            return Some(ClassifiedError {
                reason: FailoverReason::ThinkingSignature,
                action: RecoveryAction::Abort,
                description: "Assinatura de thinking inválida — requer correção no prompt".into(),
            });
        }

        if msg.contains("long context tier")
            || msg.contains("long_context_tier")
            || msg.contains("context tier required")
            || msg.contains("requires extended context")
        {
            return Some(ClassifiedError {
                reason: FailoverReason::LongContextTier,
                action: RecoveryAction::ShouldFallback,
                description: "Requisição exige tier de contexto longo não disponível".into(),
            });
        }

        if msg.contains("grammar") || msg.contains("json schema") || msg.contains("invalid tool") {
            return Some(ClassifiedError {
                reason: FailoverReason::FormatError,
                action: RecoveryAction::Abort,
                description: "Erro de formato/grammar — requer correção na requisição".into(),
            });
        }

        None
    }

    fn http_status_refinement(msg: &str) -> Option<ClassifiedError> {
        // Extrai código HTTP se presente na mensagem (ex: "401 Unauthorized", "HTTP 429")
        let code = Self::extract_http_code(msg);

        match code {
            Some(401) => Some(ClassifiedError {
                reason: FailoverReason::Auth,
                action: RecoveryAction::ShouldRotateCredential,
                description: "HTTP 401 — credencial expirada ou inválida".into(),
            }),
            Some(403) => {
                // 403 pode ser auth permanente ou policy blocked
                if msg.contains("content_policy")
                    || msg.contains("policy")
                    || msg.contains("blocked")
                    || msg.contains("violation")
                {
                    Some(ClassifiedError {
                        reason: FailoverReason::ProviderPolicyBlocked,
                        action: RecoveryAction::Abort,
                        description: "HTTP 403 — conteúdo bloqueado por política".into(),
                    })
                } else {
                    Some(ClassifiedError {
                        reason: FailoverReason::AuthPermanent,
                        action: RecoveryAction::ShouldRotateCredential,
                        description: "HTTP 403 — acesso negado (credencial permanente)".into(),
                    })
                }
            }
            Some(402) => {
                // Disambiguation: billing exhaustion vs transient usage limit
                if msg.contains("hard limit")
                    || msg.contains("exhausted")
                    || msg.contains("quota")
                    || msg.contains("billing")
                {
                    Some(ClassifiedError {
                        reason: FailoverReason::Billing,
                        action: RecoveryAction::ShouldFallback,
                        description: "HTTP 402 — faturamento esgotado".into(),
                    })
                } else {
                    Some(ClassifiedError {
                        reason: FailoverReason::RateLimit,
                        action: RecoveryAction::Retryable,
                        description: "HTTP 402 — limite de uso transitório".into(),
                    })
                }
            }
            Some(404) => {
                // Disambiguation: model not found vs provider policy blocked
                if msg.contains("model")
                    || msg.contains("not found")
                    || msg.contains("unknown model")
                {
                    Some(ClassifiedError {
                        reason: FailoverReason::ModelNotFound,
                        action: RecoveryAction::ShouldFallback,
                        description: "HTTP 404 — modelo não encontrado".into(),
                    })
                } else {
                    Some(ClassifiedError {
                        reason: FailoverReason::ProviderPolicyBlocked,
                        action: RecoveryAction::Abort,
                        description: "HTTP 404 — acesso bloqueado por política".into(),
                    })
                }
            }
            Some(408) | Some(504) => Some(ClassifiedError {
                reason: FailoverReason::Timeout,
                action: RecoveryAction::Retryable,
                description: format!("HTTP {} — timeout", code.unwrap()),
            }),
            Some(413) => Some(ClassifiedError {
                reason: FailoverReason::PayloadTooLarge,
                action: RecoveryAction::ShouldCompress,
                description: "HTTP 413 — payload muito grande".into(),
            }),
            Some(429) => {
                if msg.contains("overloaded") || msg.contains("capacity") {
                    Some(ClassifiedError {
                        reason: FailoverReason::Overloaded,
                        action: RecoveryAction::ShouldFallback,
                        description: "HTTP 429 — provider sobrecarregado".into(),
                    })
                } else {
                    Some(ClassifiedError {
                        reason: FailoverReason::RateLimit,
                        action: RecoveryAction::Retryable,
                        description: "HTTP 429 — rate limit atingido".into(),
                    })
                }
            }
            Some(500) | Some(502) | Some(503) | Some(505) => {
                if msg.contains("overloaded") || msg.contains("capacity") {
                    Some(ClassifiedError {
                        reason: FailoverReason::Overloaded,
                        action: RecoveryAction::ShouldFallback,
                        description: format!("HTTP {} — provider sobrecarregado", code.unwrap()),
                    })
                } else {
                    Some(ClassifiedError {
                        reason: FailoverReason::ServerError,
                        action: RecoveryAction::Retryable,
                        description: format!("HTTP {} — erro interno do servidor", code.unwrap()),
                    })
                }
            }
            _ => None,
        }
    }

    fn error_code_classification(msg: &str) -> Option<ClassifiedError> {
        if msg.contains("rate_limit_exceeded") || msg.contains("rate limit exceeded") {
            return Some(ClassifiedError {
                reason: FailoverReason::RateLimit,
                action: RecoveryAction::Retryable,
                description: "Código de erro: rate_limit_exceeded".into(),
            });
        }
        if msg.contains("context_length_exceeded") || msg.contains("context length exceeded") {
            return Some(ClassifiedError {
                reason: FailoverReason::ContextOverflow,
                action: RecoveryAction::ShouldCompress,
                description: "Código de erro: context_length_exceeded".into(),
            });
        }
        if msg.contains("billing_hard_limit_reached") || msg.contains("insufficient_quota") {
            return Some(ClassifiedError {
                reason: FailoverReason::Billing,
                action: RecoveryAction::ShouldFallback,
                description: "Código de erro: billing_hard_limit_reached".into(),
            });
        }
        if msg.contains("invalid_api_key") || msg.contains("invalid_api_secret") {
            return Some(ClassifiedError {
                reason: FailoverReason::AuthPermanent,
                action: RecoveryAction::ShouldRotateCredential,
                description: "Código de erro: invalid_api_key".into(),
            });
        }
        if msg.contains("model_not_found") {
            return Some(ClassifiedError {
                reason: FailoverReason::ModelNotFound,
                action: RecoveryAction::ShouldFallback,
                description: "Código de erro: model_not_found".into(),
            });
        }
        if msg.contains("content_policy_violation") {
            return Some(ClassifiedError {
                reason: FailoverReason::ProviderPolicyBlocked,
                action: RecoveryAction::Abort,
                description: "Código de erro: content_policy_violation".into(),
            });
        }
        if msg.contains("payload_too_large") {
            return Some(ClassifiedError {
                reason: FailoverReason::PayloadTooLarge,
                action: RecoveryAction::ShouldCompress,
                description: "Código de erro: payload_too_large".into(),
            });
        }
        if msg.contains("image_too_large") || msg.contains("image too large") {
            return Some(ClassifiedError {
                reason: FailoverReason::ImageTooLarge,
                action: RecoveryAction::ShouldCompress,
                description: "Código de erro: image_too_large".into(),
            });
        }

        None
    }

    fn message_pattern_matching(msg: &str) -> Option<ClassifiedError> {
        if msg.contains("timeout") || msg.contains("timed out") || msg.contains("time out") {
            return Some(ClassifiedError {
                reason: FailoverReason::Timeout,
                action: RecoveryAction::Retryable,
                description: "Timeout detectado na mensagem".into(),
            });
        }

        if msg.contains("overloaded")
            || msg.contains("too many requests")
            || msg.contains("capacity exceeded")
            || msg.contains("temporarily unavailable")
        {
            return Some(ClassifiedError {
                reason: FailoverReason::Overloaded,
                action: RecoveryAction::ShouldFallback,
                description: "Provider sobrecarregado".into(),
            });
        }

        if msg.contains("context")
            && (msg.contains("token limit")
                || msg.contains("max tokens")
                || msg.contains("too long")
                || msg.contains("window"))
        {
            return Some(ClassifiedError {
                reason: FailoverReason::ContextOverflow,
                action: RecoveryAction::ShouldCompress,
                description: "Limite de contexto excedido".into(),
            });
        }

        if msg.contains("image") && (msg.contains("too large") || msg.contains("file too large")) {
            return Some(ClassifiedError {
                reason: FailoverReason::ImageTooLarge,
                action: RecoveryAction::ShouldCompress,
                description: "Imagem muito grande".into(),
            });
        }

        if msg.contains("payload") && msg.contains("too large") {
            return Some(ClassifiedError {
                reason: FailoverReason::PayloadTooLarge,
                action: RecoveryAction::ShouldCompress,
                description: "Payload muito grande".into(),
            });
        }

        if msg.contains("billing") || msg.contains("quota") || msg.contains("payment") {
            return Some(ClassifiedError {
                reason: FailoverReason::Billing,
                action: RecoveryAction::ShouldFallback,
                description: "Problema de faturamento".into(),
            });
        }

        if msg.contains("rate limit") || msg.contains("throttled") || msg.contains("throttling") {
            return Some(ClassifiedError {
                reason: FailoverReason::RateLimit,
                action: RecoveryAction::Retryable,
                description: "Rate limit detectado".into(),
            });
        }

        if msg.contains("invalid api key")
            || msg.contains("unauthorized")
            || msg.contains("authentication failed")
        {
            return Some(ClassifiedError {
                reason: FailoverReason::Auth,
                action: RecoveryAction::ShouldRotateCredential,
                description: "Falha de autenticação".into(),
            });
        }

        if msg.contains("content policy")
            || msg.contains("safety")
            || msg.contains("blocked")
            || msg.contains("moderation")
        {
            return Some(ClassifiedError {
                reason: FailoverReason::ProviderPolicyBlocked,
                action: RecoveryAction::Abort,
                description: "Conteúdo bloqueado por política".into(),
            });
        }

        if msg.contains("model not found") || msg.contains("unknown model") {
            return Some(ClassifiedError {
                reason: FailoverReason::ModelNotFound,
                action: RecoveryAction::ShouldFallback,
                description: "Modelo não encontrado".into(),
            });
        }

        if msg.contains("format") || msg.contains("json") || msg.contains("schema") {
            return Some(ClassifiedError {
                reason: FailoverReason::FormatError,
                action: RecoveryAction::Abort,
                description: "Erro de formato".into(),
            });
        }

        None
    }

    fn ssl_tls_transient(msg: &str) -> Option<ClassifiedError> {
        if msg.contains("ssl")
            || msg.contains("tls")
            || msg.contains("certificate")
            || msg.contains("handshake")
        {
            return Some(ClassifiedError {
                reason: FailoverReason::Timeout,
                action: RecoveryAction::Retryable,
                description: "Erro SSL/TLS tratado como transitório (timeout)".into(),
            });
        }
        None
    }

    fn server_disconnect_context(msg: &str, large_session: bool) -> Option<ClassifiedError> {
        if large_session
            && (msg.contains("disconnect")
                || msg.contains("connection reset")
                || msg.contains("broken pipe")
                || msg.contains("connection closed")
                || msg.contains("reset by peer"))
        {
            return Some(ClassifiedError {
                reason: FailoverReason::ContextOverflow,
                action: RecoveryAction::ShouldCompress,
                description: "Desconexão do servidor com sessão grande — provável context overflow"
                    .into(),
            });
        }
        None
    }

    // ------------------------------------------------------------------
    // Utilitários
    // ------------------------------------------------------------------

    fn extract_http_code(msg: &str) -> Option<u16> {
        // Tenta encontrar padrões como "401", "HTTP 429", "status 500"
        let prefixes = ["http ", "status ", "error ", "code "];
        for prefix in &prefixes {
            if let Some(pos) = msg.find(prefix) {
                let start = pos + prefix.len();
                let rest = &msg[start..];
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(code) = digits.parse::<u16>() {
                    if (100..=599).contains(&code) {
                        return Some(code);
                    }
                }
            }
        }

        // Fallback: procura qualquer número de 3 dígitos no início ou após espaço
        let mut chars = msg.chars().peekable();
        while chars.peek().is_some() {
            let chunk: String = chars.by_ref().take_while(|c| !c.is_whitespace()).collect();
            if chunk.len() == 3 && chunk.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(code) = chunk.parse::<u16>() {
                    if (100..=599).contains(&code) {
                        return Some(code);
                    }
                }
            }
        }

        None
    }
}

// ===================================================================
// Testes
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn classify(msg: &str) -> ClassifiedError {
        let err = anyhow::anyhow!("{}", msg);
        ErrorClassifier::classify(&err)
    }

    fn classify_with_ctx(msg: &str, tokens: u64, max_ctx: u64) -> ClassifiedError {
        let err = anyhow::anyhow!("{}", msg);
        ErrorClassifier::classify_with_context(&err, tokens, max_ctx)
    }

    #[test]
    fn thinking_signature_abort() {
        let c = classify("thinking signature mismatch in response");
        assert_eq!(c.reason, FailoverReason::ThinkingSignature);
        assert_eq!(c.action, RecoveryAction::Abort);
    }

    #[test]
    fn long_context_tier_fallback() {
        let c = classify("long_context_tier required for this model");
        assert_eq!(c.reason, FailoverReason::LongContextTier);
        assert_eq!(c.action, RecoveryAction::ShouldFallback);
    }

    #[test]
    fn grammar_format_error_abort() {
        let c = classify("invalid grammar in structured output request");
        assert_eq!(c.reason, FailoverReason::FormatError);
        assert_eq!(c.action, RecoveryAction::Abort);
    }

    #[test]
    fn http_401_auth_rotate() {
        let c = classify("HTTP 401 Unauthorized");
        assert_eq!(c.reason, FailoverReason::Auth);
        assert_eq!(c.action, RecoveryAction::ShouldRotateCredential);
    }

    #[test]
    fn http_403_policy_blocked() {
        let c = classify("HTTP 403 content_policy_violation");
        assert_eq!(c.reason, FailoverReason::ProviderPolicyBlocked);
        assert_eq!(c.action, RecoveryAction::Abort);
    }

    #[test]
    fn http_403_auth_permanent() {
        let c = classify("HTTP 403 Forbidden");
        assert_eq!(c.reason, FailoverReason::AuthPermanent);
        assert_eq!(c.action, RecoveryAction::ShouldRotateCredential);
    }

    #[test]
    fn http_402_billing_exhausted() {
        let c = classify("HTTP 402 billing hard limit reached");
        assert_eq!(c.reason, FailoverReason::Billing);
        assert_eq!(c.action, RecoveryAction::ShouldFallback);
    }

    #[test]
    fn http_402_transient_usage_limit() {
        let c = classify("HTTP 402 usage limit transient");
        assert_eq!(c.reason, FailoverReason::RateLimit);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn http_404_model_not_found() {
        let c = classify("HTTP 404 model not found");
        assert_eq!(c.reason, FailoverReason::ModelNotFound);
        assert_eq!(c.action, RecoveryAction::ShouldFallback);
    }

    #[test]
    fn http_404_policy_blocked() {
        let c = classify("HTTP 404 blocked by provider policy");
        assert_eq!(c.reason, FailoverReason::ProviderPolicyBlocked);
        assert_eq!(c.action, RecoveryAction::Abort);
    }

    #[test]
    fn http_429_rate_limit() {
        let c = classify("HTTP 429 rate limit exceeded");
        assert_eq!(c.reason, FailoverReason::RateLimit);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn http_429_overloaded() {
        let c = classify("HTTP 429 overloaded capacity");
        assert_eq!(c.reason, FailoverReason::Overloaded);
        assert_eq!(c.action, RecoveryAction::ShouldFallback);
    }

    #[test]
    fn http_500_server_error_retryable() {
        let c = classify("HTTP 500 internal server error");
        assert_eq!(c.reason, FailoverReason::ServerError);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn http_413_compress() {
        let c = classify("HTTP 413 payload too large");
        assert_eq!(c.reason, FailoverReason::PayloadTooLarge);
        assert_eq!(c.action, RecoveryAction::ShouldCompress);
    }

    #[test]
    fn error_code_context_overflow_compress() {
        let c = classify("error code context_length_exceeded");
        assert_eq!(c.reason, FailoverReason::ContextOverflow);
        assert_eq!(c.action, RecoveryAction::ShouldCompress);
    }

    #[test]
    fn error_code_invalid_api_key_permanent() {
        let c = classify("invalid_api_key — authentication failed");
        assert_eq!(c.reason, FailoverReason::AuthPermanent);
        assert_eq!(c.action, RecoveryAction::ShouldRotateCredential);
    }

    #[test]
    fn ssl_tls_timeout_retryable() {
        let c = classify("TLS handshake failed with provider");
        assert_eq!(c.reason, FailoverReason::Timeout);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn server_disconnect_large_session_context_overflow() {
        let c = classify_with_ctx("connection reset by peer", 5000, 8000);
        assert_eq!(c.reason, FailoverReason::ContextOverflow);
        assert_eq!(c.action, RecoveryAction::ShouldCompress);
    }

    #[test]
    fn server_disconnect_small_session_not_context_overflow() {
        let c = classify_with_ctx("connection reset by peer", 100, 8000);
        assert_eq!(c.reason, FailoverReason::ServerError);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn fallback_unknown_retryable() {
        let c = classify("something completely unexpected happened");
        assert_eq!(c.reason, FailoverReason::ServerError);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn message_pattern_billing_fallback() {
        let c = classify("your quota has been exceeded");
        assert_eq!(c.reason, FailoverReason::Billing);
        assert_eq!(c.action, RecoveryAction::ShouldFallback);
    }

    #[test]
    fn message_pattern_timeout_retryable() {
        let c = classify("request timed out after 30s");
        assert_eq!(c.reason, FailoverReason::Timeout);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }

    #[test]
    fn message_pattern_image_too_large_compress() {
        let c = classify("image too large for this model");
        assert_eq!(c.reason, FailoverReason::ImageTooLarge);
        assert_eq!(c.action, RecoveryAction::ShouldCompress);
    }

    #[test]
    fn message_pattern_policy_blocked_abort() {
        let c = classify("content blocked by safety moderation");
        assert_eq!(c.reason, FailoverReason::ProviderPolicyBlocked);
        assert_eq!(c.action, RecoveryAction::Abort);
    }

    #[test]
    fn http_502_overloaded() {
        let c = classify("HTTP 502 overloaded");
        assert_eq!(c.reason, FailoverReason::Overloaded);
        assert_eq!(c.action, RecoveryAction::ShouldFallback);
    }

    #[test]
    fn http_408_timeout() {
        let c = classify("HTTP 408 Request Timeout");
        assert_eq!(c.reason, FailoverReason::Timeout);
        assert_eq!(c.action, RecoveryAction::Retryable);
    }
}
