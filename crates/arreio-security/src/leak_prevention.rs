use anyhow::{anyhow, Result};
use arreio_provider::{ChatRequest, ChatResponse};
use std::collections::HashSet;

use crate::dlp::DlpEngine;

/// Bloqueio proativo de vazamento de dados sensíveis.
/// Intercepta requisições e respostas de chat aplicando regras DLP.
pub struct LeakPrevention {
    dlp: DlpEngine,
    blocked_patterns: HashSet<String>,
}

impl LeakPrevention {
    /// Cria uma nova instância com os padrões DLP padrão.
    pub fn new() -> Self {
        Self {
            dlp: DlpEngine::with_defaults(),
            blocked_patterns: HashSet::new(),
        }
    }

    /// Adiciona o nome de um padrão à lista de bloqueio explícito.
    pub fn block_pattern(&mut self, name: impl Into<String>) {
        self.blocked_patterns.insert(name.into());
    }

    /// Intercepta uma `ChatRequest` antes de enviá-la ao provider.
    /// Retorna erro se dados sensíveis forem detectados no system ou user prompt.
    pub fn intercept_request(&self, req: &ChatRequest) -> Result<()> {
        let combined = format!("{} {}", req.system, req.user);
        let matches = self.dlp.scan(&combined);
        if let Some(m) = matches.first() {
            return Err(anyhow!(
                "DLP: dados sensíveis detectados na requisição (pattern: {})",
                m.pattern_name
            ));
        }
        Ok(())
    }

    /// Intercepta uma `ChatResponse` antes de retorná-la ao usuário.
    /// Mascara dados sensíveis encontrados no conteúdo ou nos argumentos de tool calls.
    pub fn intercept_response(&self, resp: &mut ChatResponse) -> Result<()> {
        resp.content = self.redact_text(&resp.content);

        if let Some(ref mut calls) = resp.tool_calls {
            for call in calls.iter_mut() {
                call.function.arguments = self.redact_text(&call.function.arguments);
            }
        }

        Ok(())
    }

    /// Retorna `true` se o padrão estiver na lista de bloqueio explícito.
    pub fn is_blocked(&self, pattern_name: &str) -> bool {
        self.blocked_patterns.contains(pattern_name)
    }

    /// Mascara ocorrências sensíveis em um texto, substituindo por `[REDACTED:<nome>]`.
    fn redact_text(&self, text: &str) -> String {
        let mut matches = self.dlp.scan(text);
        if matches.is_empty() {
            return text.to_string();
        }
        // Substitui do final para o início para não invalidar os ranges
        matches.sort_by_key(|m| std::cmp::Reverse(m.position.0));
        let mut output = text.to_string();
        for m in matches {
            let mask = format!("[REDACTED:{}]", m.pattern_name);
            output.replace_range(m.position.0..m.position.1, &mask);
        }
        output
    }
}

impl Default for LeakPrevention {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_provider::{ToolCall, ToolCallFunction};

    fn lp() -> LeakPrevention {
        LeakPrevention::new()
    }

    fn req(system: &str, user: &str) -> ChatRequest {
        ChatRequest {
            messages: Vec::new(),
            model: "test".to_string(),
            system: system.to_string(),
            user: user.to_string(),
            tools: None,
        }
    }

    fn resp(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.to_string(),
            tool_calls: None,
            tokens_in: 0,
            tokens_out: 0,
            rate_limit: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn intercept_request_clean() {
        let lp = lp();
        let r = req("system", "Diga oi");
        assert!(lp.intercept_request(&r).is_ok());
    }

    #[test]
    fn intercept_request_blocks_cpf() {
        let lp = lp();
        let r = req("system", "Meu CPF é 529.982.247-25");
        let err = lp.intercept_request(&r).unwrap_err();
        assert!(err.to_string().contains("CPF"));
    }

    #[test]
    fn intercept_request_blocks_email() {
        let lp = lp();
        let r = req("system", "Contato: joao@example.com");
        let err = lp.intercept_request(&r).unwrap_err();
        assert!(err.to_string().contains("Email"));
    }

    #[test]
    fn intercept_request_blocks_api_key() {
        let lp = lp();
        let r = req("system", "api_key = 'abcdef1234567890abcdef12'");
        let err = lp.intercept_request(&r).unwrap_err();
        assert!(err.to_string().contains("APIKey"));
    }

    #[test]
    fn intercept_request_blocks_system_prompt() {
        let lp = lp();
        let r = req("AKIAIOSFODNN7EXAMPLE", "ok");
        let err = lp.intercept_request(&r).unwrap_err();
        assert!(err.to_string().contains("AWSAccessKey"));
    }

    #[test]
    fn intercept_response_no_change_when_clean() {
        let lp = lp();
        let mut r = resp("Texto completamente inofensivo.");
        lp.intercept_response(&mut r).unwrap();
        assert_eq!(r.content, "Texto completamente inofensivo.");
    }

    #[test]
    fn intercept_response_masks_cpf() {
        let lp = lp();
        let mut r = resp("CPF do cliente: 529.982.247-25. Obrigado.");
        lp.intercept_response(&mut r).unwrap();
        assert!(r.content.contains("[REDACTED:CPF]"));
        assert!(!r.content.contains("529.982.247-25"));
    }

    #[test]
    fn intercept_response_masks_api_key() {
        let lp = lp();
        let mut r = resp("Use token: secret_1234567890123456");
        lp.intercept_response(&mut r).unwrap();
        assert!(r.content.contains("[REDACTED:APIKey]"));
        assert!(!r.content.contains("secret_1234567890123456"));
    }

    #[test]
    fn intercept_response_multiple_masks() {
        let lp = lp();
        let mut r = resp("Email: ana@example.com e CPF 111.222.333-44");
        lp.intercept_response(&mut r).unwrap();
        assert!(r.content.contains("[REDACTED:Email]"));
        assert!(r.content.contains("[REDACTED:CPF]"));
        assert!(!r.content.contains("ana@example.com"));
        assert!(!r.content.contains("111.222.333-44"));
    }

    #[test]
    fn intercept_response_tool_calls_arguments() {
        let lp = lp();
        let mut r = ChatResponse {
            content: "ok".to_string(),
            tool_calls: Some(vec![ToolCall {
                id: "1".to_string(),
                r#type: "function".to_string(),
                function: ToolCallFunction {
                    name: "send_email".to_string(),
                    arguments: r#"{"cmd":"set api_key=abcdef1234567890abcdef12"}"#.to_string(),
                },
            }]),
            tokens_in: 0,
            tokens_out: 0,
            rate_limit: None,
            reasoning_content: None,
        };
        lp.intercept_response(&mut r).unwrap();
        let args = &r.tool_calls.as_ref().unwrap()[0].function.arguments;
        assert!(args.contains("[REDACTED:APIKey]"));
        assert!(!args.contains("abcdef1234567890abcdef12"));
    }

    #[test]
    fn blocked_patterns_custom() {
        let mut lp = lp();
        lp.block_pattern("Email");
        assert!(lp.is_blocked("Email"));
        assert!(!lp.is_blocked("CPF"));
    }

    #[test]
    fn intercept_request_blocks_credit_card() {
        let lp = lp();
        let r = req("system", "Cartão: 4111 1111 1111 1111");
        let err = lp.intercept_request(&r).unwrap_err();
        assert!(err.to_string().contains("CreditCard"));
    }

    #[test]
    fn intercept_response_masks_ssn() {
        let lp = lp();
        let mut r = resp("SSN é 123-45-6789");
        lp.intercept_response(&mut r).unwrap();
        assert!(r.content.contains("[REDACTED:SSN]"));
        assert!(!r.content.contains("123-45-6789"));
    }
}
