//! Hook wrapper para chamadas LLM.
//!
//! Permite interceptar requisições e respostas LLM sem modificar os atores.
//! O `HookedProvider` wrapa qualquer `ProviderClient` e dispara callbacks
//! antes (`pre`) e depois (`post`) de cada chamada `chat`.

use anyhow::Result;

use crate::{ChatRequest, ChatResponse, ProviderClient};

/// Callback chamado antes de uma requisição LLM.
/// Recebe o request JSON. Pode transformar ou abortar retornando Err.
/// Usa `Arc` para permitir clone entre instâncias de `HookedProvider`.
pub type PreLlmHook = std::sync::Arc<dyn Fn(&mut ChatRequest) -> Result<()> + Send + Sync>;

/// Callback chamado após uma resposta LLM.
/// Recebe o request original e a response. Pode transformar a response.
/// Usa `Arc` para permitir clone entre instâncias de `HookedProvider`.
pub type PostLlmHook = std::sync::Arc<dyn Fn(&ChatRequest, &mut ChatResponse) -> Result<()> + Send + Sync>;

/// ProviderClient que dispara hooks antes e depois da chamada real.
pub struct HookedProvider {
    inner: Box<dyn ProviderClient>,
    pre: Option<PreLlmHook>,
    post: Option<PostLlmHook>,
}

impl HookedProvider {
    pub fn new(inner: Box<dyn ProviderClient>) -> Self {
        Self {
            inner,
            pre: None,
            post: None,
        }
    }

    pub fn with_pre(mut self, hook: PreLlmHook) -> Self {
        self.pre = Some(hook);
        self
    }

    pub fn with_post(mut self, hook: PostLlmHook) -> Self {
        self.post = Some(hook);
        self
    }

    /// Clona o provider preservando hooks LLM via Arc.
    fn clone_hooks(&self) -> (Option<PreLlmHook>, Option<PostLlmHook>) {
        (self.pre.clone(), self.post.clone())
    }
}

impl ProviderClient for HookedProvider {
    fn chat(&self, mut req: ChatRequest) -> Result<ChatResponse> {
        if let Some(ref pre) = self.pre {
            pre(&mut req)?;
        }
        let mut resp = self.inner.chat(req.clone())?;
        if let Some(ref post) = self.post {
            post(&req, &mut resp)?;
        }
        Ok(resp)
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        let (pre, post) = self.clone_hooks();
        Box::new(Self {
            inner: self.inner.clone_box(),
            pre,
            post,
        })
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        self.inner.cost_estimate(input_tokens, output_tokens)
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.inner.embed(texts)
    }

    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        self.inner.chat_stream(req)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;
    use std::sync::Arc;

    #[test]
    fn pre_hook_transforma_request() {
        let inner = MockProvider::new("resposta");
        let hooked = HookedProvider::new(Box::new(inner)).with_pre(Arc::new(|req| {
            req.system = "transformado".to_string();
            Ok(())
        }));
        let req = ChatRequest {
            messages: Vec::new(),
            model: "m".to_string(),
            system: "original".to_string(),
            user: "u".to_string(),
            tools: None,
        };
        let resp = hooked.chat(req).unwrap();
        assert_eq!(resp.content, "resposta");
    }

    #[test]
    fn post_hook_transforma_response() {
        let inner = MockProvider::new("resposta");
        let hooked = HookedProvider::new(Box::new(inner)).with_post(Arc::new(|_req: &ChatRequest, resp: &mut crate::ChatResponse| {
            resp.content = format!("[hooked] {}", resp.content);
            Ok(())
        }));
        let req = ChatRequest {
            messages: Vec::new(),
            model: "m".to_string(),
            system: "s".to_string(),
            user: "u".to_string(),
            tools: None,
        };
        let resp = hooked.chat(req).unwrap();
        assert_eq!(resp.content, "[hooked] resposta");
    }

    #[test]
    fn pre_hook_pode_abortar() {
        let inner = MockProvider::new("resposta");
        let hooked = HookedProvider::new(Box::new(inner))
            .with_pre(Arc::new(|_req: &mut ChatRequest| Err(anyhow::anyhow!("abortado pelo hook"))));
        let req = ChatRequest {
            messages: Vec::new(),
            model: "m".to_string(),
            system: "s".to_string(),
            user: "u".to_string(),
            tools: None,
        };
        assert!(hooked.chat(req).is_err());
    }
}
