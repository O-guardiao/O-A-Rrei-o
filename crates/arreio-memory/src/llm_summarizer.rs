//! Sumarizador baseado em LLM para ContextCollapser (P-007).
//!
//! Usa um `ProviderClient` para gerar sumários textuais de entradas colapsadas.
//! Em caso de falha (timeout, erro de API), retorna `None` para fallback
//! heurístico no ContextCollapser.

use crate::context_collapse::Summarizer;
use arreio_provider::{ChatRequest, ProviderClient};
use serde_json::Value;

/// Sumarizador que delega a um LLM via ProviderClient.
pub struct LlmSummarizer {
    client: Box<dyn ProviderClient>,
    model: String,
}

impl LlmSummarizer {
    /// Cria um novo sumarizador com o provider e modelo especificados.
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }
}

impl Summarizer for LlmSummarizer {
    fn summarize(&self, category: &str, entries: &[(String, Value)]) -> Option<String> {
        let n = entries.len();
        if n == 0 {
            return Some("Nenhuma entrada para sumarizar.".to_string());
        }

        // Monta um resumo compacto das entradas para o prompt
        let mut prompt_lines = vec![format!(
            "Resuma em 3 frases curtas o que aconteceu nestas {} entradas de '{}':",
            n, category
        )];
        for (i, (key, val)) in entries.iter().take(20).enumerate() {
            let preview = val.to_string();
            let truncated = if preview.len() > 200 {
                format!("{}...", &preview[..200])
            } else {
                preview
            };
            prompt_lines.push(format!("{}: {} -> {}", i + 1, key, truncated));
        }
        if n > 20 {
            prompt_lines.push(format!("... e mais {} entradas.", n - 20));
        }
        let user_prompt = prompt_lines.join("\n");

        let req = ChatRequest {
            model: self.model.clone(),
            system: "Você é um sumarizador conciso. Responda em português com no máximo 3 frases curtas.".to_string(),
            user: user_prompt,
            messages: vec![],
            tools: None,
        };

        match self.client.chat(req) {
            Ok(resp) => {
                let summary = resp.content.trim().to_string();
                if summary.is_empty() {
                    None
                } else {
                    Some(summary)
                }
            }
            Err(e) => {
                eprintln!(
                    "[llm_summarizer] Falha ao sumarizar {} entradas de '{}': {}",
                    n, category, e
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_provider::MockProvider;

    #[test]
    fn llm_summarizer_returns_content_from_provider() {
        let provider = Box::new(MockProvider::new("Sumário gerado pelo mock."));
        let summarizer = LlmSummarizer::new(provider, "mock:test");
        let entries = vec![
            ("key1".to_string(), serde_json::json!({"result": "ok"})),
            ("key2".to_string(), serde_json::json!({"result": "ok"})),
        ];
        let result = summarizer.summarize("dag", &entries);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "Sumário gerado pelo mock.");
    }

    #[test]
    fn llm_summarizer_returns_none_on_empty() {
        let provider = Box::new(MockProvider::new("não importa"));
        let summarizer = LlmSummarizer::new(provider, "mock:test");
        let result = summarizer.summarize("dag", &[]);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "Nenhuma entrada para sumarizar.");
    }
}
