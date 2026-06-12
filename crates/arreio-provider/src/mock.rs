use crate::provider::{ChatRequest, ChatResponse, ProviderClient};
use anyhow::bail;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Provedor mock para testes determinísticos.
/// Mapeia substrings do prompt do usuário para respostas fixas.
pub struct MockProvider {
    scenarios: Arc<Mutex<HashMap<String, String>>>,
    default_response: String,
    fail_count: Arc<Mutex<u32>>,
    max_failures: u32,
}

impl MockProvider {
    pub fn new(default_response: impl Into<String>) -> Self {
        Self {
            scenarios: Arc::new(Mutex::new(HashMap::new())),
            default_response: default_response.into(),
            fail_count: Arc::new(Mutex::new(0)),
            max_failures: 0,
        }
    }

    /// Cria um mock que falha nas primeiras `n` chamadas, depois responde normalmente.
    pub fn with_failures(n: u32) -> Self {
        Self {
            scenarios: Arc::new(Mutex::new(HashMap::new())),
            default_response: "ok".into(),
            fail_count: Arc::new(Mutex::new(0)),
            max_failures: n,
        }
    }

    /// Registra um cenário: se o prompt do usuário contém `trigger`, retorna `response`.
    pub fn when(&self, trigger: impl Into<String>, response: impl Into<String>) {
        self.scenarios
            .lock()
            .unwrap()
            .insert(trigger.into(), response.into());
    }
}

impl Clone for MockProvider {
    fn clone(&self) -> Self {
        Self {
            scenarios: Arc::new(Mutex::new(self.scenarios.lock().unwrap().clone())),
            default_response: self.default_response.clone(),
            fail_count: Arc::new(Mutex::new(*self.fail_count.lock().unwrap())),
            max_failures: self.max_failures,
        }
    }
}

impl ProviderClient for MockProvider {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        Box::new(self.clone())
    }

    fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
        0.0
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        // Retorna embeddings dummy: vetor de 4 floats derivado do comprimento do texto.
        let mut result = Vec::with_capacity(texts.len());
        for text in texts {
            let len = text.len() as f32;
            result.push(vec![len, len / 2.0, len / 4.0, len / 8.0]);
        }
        Ok(result)
    }

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let mut fails = self.fail_count.lock().unwrap();
        if *fails < self.max_failures {
            *fails += 1;
            return Err(anyhow::anyhow!(
                "mock failure {}/{}",
                *fails,
                self.max_failures
            ));
        }
        drop(fails);

        let scenarios = self.scenarios.lock().unwrap();
        let content = scenarios
            .iter()
            .find(|(trigger, _)| req.user.contains(*trigger))
            .map(|(_, response)| response.clone())
            .unwrap_or_else(|| self.default_response.clone());

        let tokens_out = content.split_whitespace().count() as u64;
        Ok(ChatResponse {
            content,
            tool_calls: None,
            tokens_in: req.user.len() as u64 / 4,
            tokens_out,
            rate_limit: None,
            reasoning_content: None,
        })
    }
    fn chat_stream(
        &self,
        _req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        bail!("streaming não suportado por este provider")
    }
}

/// Cria um ToolCall mock para testes de tool-use.
pub fn mock_tool_call(name: &str, arguments: &str) -> crate::provider::ToolCall {
    crate::provider::ToolCall {
        id: format!("call_{}", uuid::Uuid::new_v4()),
        r#type: "function".to_string(),
        function: crate::provider::ToolCallFunction {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_responde_cenario_registrado() {
        let mock = MockProvider::new("resposta padrão");
        mock.when("status", "{'status': 'ok'}");

        let resp = mock
            .chat(ChatRequest {
                messages: Vec::new(),
                model: "mock".into(),
                system: "sys".into(),
                user: "qual o status?".into(),
                tools: None,
            })
            .unwrap();

        assert_eq!(resp.content, "{'status': 'ok'}");
        assert!(resp.tokens_out > 0);
    }

    #[test]
    fn mock_responde_padrao_quando_sem_match() {
        let mock = MockProvider::new("resposta padrão");
        let resp = mock
            .chat(ChatRequest {
                messages: Vec::new(),
                model: "mock".into(),
                system: "sys".into(),
                user: "algo inesperado".into(),
                tools: None,
            })
            .unwrap();

        assert_eq!(resp.content, "resposta padrão");
    }
}
