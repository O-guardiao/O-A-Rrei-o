//! Integração entre arreio-recovery e arreio-provider.
//!
//! Provê `RecoveryBlockPoolBuilder` para montar um `RecoveryBlockManager`
//! com fallback real entre providers LLM (GPT-4, Claude, Gemini, Ollama).

use anyhow::Result;
use arreio_provider::{
    AnthropicProvider, GoogleProvider, OllamaProvider, OpenAiCompatProvider, RecoveryBlockManager,
};

/// Builder que constrói um `RecoveryBlockManager` pré-configurado com
/// diversidade de providers reais para recovery blocks.
pub struct RecoveryBlockPoolBuilder;

impl RecoveryBlockPoolBuilder {
    /// Cria um `RecoveryBlockManager` com fallback entre providers reais.
    ///
    /// Primary: GPT-4o (OpenAI)  
    /// Alternate 1: Claude 3.5 Sonnet (Anthropic) — se `ANTHROPIC_API_KEY` estiver definida  
    /// Alternate 2: Gemini 1.5 Pro (Google) — se `GOOGLE_API_KEY` estiver definida  
    /// Alternate 3: Ollama local (gemma4:latest)
    pub fn default_multi_model() -> Result<RecoveryBlockManager> {
        let primary = Box::new(OpenAiCompatProvider::new(
            "api.openai.com",
            443,
            std::env::var("OPENAI_API_KEY").ok(),
            true,
        ));

        let mut mgr = RecoveryBlockManager::new(primary);

        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            mgr = mgr.add_alternate(Box::new(AnthropicProvider::new(
                "api.anthropic.com",
                443,
                key,
                true,
            )));
        }

        if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
            mgr = mgr.add_alternate(Box::new(GoogleProvider::new(
                key,
                "gemini-1.5-pro".to_string(),
            )));
        }

        let bb = temp_blackboard()?;
        mgr = mgr.add_alternate(Box::new(OllamaProvider::new(bb)));

        Ok(mgr)
    }
}

/// Cria um `Blackboard` temporário para uso com `OllamaProvider`.
fn temp_blackboard() -> Result<arreio_kernel::Blackboard> {
    let tmp = tempfile::NamedTempFile::new()?;
    let path = tmp.path().to_path_buf();
    drop(tmp);
    Ok(arreio_kernel::Blackboard::open(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_provider::{
        AcceptanceResult, AcceptanceTest, ChatRequest, ChatResponse, FailoverStrategy,
        ProviderClient, RecoveryBlockManager,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Mock de provider para testes determinísticos de fallback e custo.
    struct NamedMock {
        name: &'static str,
        cost: f64,
        result: Result<String, &'static str>,
        calls: Arc<AtomicUsize>,
    }

    impl NamedMock {
        fn new(name: &'static str, cost: f64, result: Result<String, &'static str>) -> Self {
            Self {
                name,
                cost,
                result,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Clone for NamedMock {
        fn clone(&self) -> Self {
            Self {
                name: self.name,
                cost: self.cost,
                result: self.result.clone(),
                calls: Arc::clone(&self.calls),
            }
        }
    }

    impl ProviderClient for NamedMock {
        fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.result {
                Ok(content) => Ok(ChatResponse {
                    content: content.clone(),
                    tool_calls: None,
                    tokens_in: 1,
                    tokens_out: 1,
                    rate_limit: None,
                    reasoning_content: None,
                }),
                Err(msg) => Err(anyhow::anyhow!("{}", msg)),
            }
        }

        fn name(&self) -> &'static str {
            self.name
        }

        fn clone_box(&self) -> Box<dyn ProviderClient> {
            Box::new(self.clone())
        }

        fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
            self.cost
        }

        fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            Err(anyhow::anyhow!("embed não suportado"))
        }

        fn chat_stream(
            &self,
            _req: ChatRequest,
        ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
            Err(anyhow::anyhow!("streaming não suportado"))
        }
    }

    /// Teste de aceitação que sempre passa.
    struct AlwaysPass;
    impl AcceptanceTest for AlwaysPass {
        fn evaluate(&self, _response: &ChatResponse, _request: &ChatRequest) -> AcceptanceResult {
            AcceptanceResult::Pass
        }
    }

    fn dummy_request() -> ChatRequest {
        ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "sys".into(),
            user: "user".into(),
            tools: None,
        }
    }

    #[test]
    fn builder_cria_manager_sem_panico() {
        let mgr = RecoveryBlockPoolBuilder::default_multi_model();
        assert!(mgr.is_ok());
    }

    #[test]
    fn fallback_para_alternate_quando_primary_falha() {
        let primary = NamedMock::new("primary", 1.0, Err("falha primária"));
        let alternate = NamedMock::new("alternate", 0.5, Ok("sucesso".into()));

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(AlwaysPass))
            .add_alternate(Box::new(alternate.clone()));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "alternate");
        assert_eq!(result.response.content, "sucesso");
        assert_eq!(result.attempts, 2);
        assert_eq!(alternate.call_count(), 1);
    }

    #[test]
    fn primario_sucesso_nao_chama_alternates() {
        let primary = NamedMock::new("primary", 1.0, Ok("ok".into()));
        let alternate = NamedMock::new("alternate", 0.1, Ok("nunca".into()));

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(AlwaysPass))
            .add_alternate(Box::new(alternate.clone()));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "primary");
        assert_eq!(result.attempts, 1);
        assert_eq!(alternate.call_count(), 0);
    }

    #[test]
    fn cost_optimized_ordenado_pelo_menor_custo() {
        let primary = NamedMock::new("primary", 10.0, Err("falha"));
        let caro = NamedMock::new("caro", 5.0, Ok("caro ok".into()));
        let barato = NamedMock::new("barato", 0.1, Ok("barato ok".into()));

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(AlwaysPass))
            .with_failover_strategy(FailoverStrategy::CostOptimized)
            .add_alternate(Box::new(caro.clone()))
            .add_alternate(Box::new(barato.clone()));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "barato");
        assert_eq!(result.attempts, 2);
        assert_eq!(barato.call_count(), 1);
        assert_eq!(caro.call_count(), 0);
    }

    #[test]
    fn cost_optimized_tenta_seguinte_se_mais_barato_falhar() {
        let primary = NamedMock::new("primary", 10.0, Err("falha"));
        let barato = NamedMock::new("barato", 0.1, Err("falha barato"));
        let caro = NamedMock::new("caro", 5.0, Ok("caro ok".into()));

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(AlwaysPass))
            .with_failover_strategy(FailoverStrategy::CostOptimized)
            .add_alternate(Box::new(barato))
            .add_alternate(Box::new(caro));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "caro");
        assert_eq!(result.attempts, 3);
    }

    #[test]
    fn priority_mantem_ordem_de_registro() {
        let primary = NamedMock::new("primary", 10.0, Err("falha"));
        let alt1 = NamedMock::new("alt1", 0.1, Err("falha alt1"));
        let alt2 = NamedMock::new("alt2", 100.0, Ok("alt2 ok".into()));

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(AlwaysPass))
            .with_failover_strategy(FailoverStrategy::Priority)
            .add_alternate(Box::new(alt1))
            .add_alternate(Box::new(alt2.clone()));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "alt2");
        assert_eq!(result.attempts, 3);
    }

    #[test]
    fn todos_providers_falham_retorna_erro() {
        let primary = NamedMock::new("primary", 1.0, Err("falha"));
        let alt1 = NamedMock::new("alt1", 0.5, Err("falha"));

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(AlwaysPass))
            .add_alternate(Box::new(alt1));

        let err = mgr.execute(dummy_request()).unwrap_err().to_string();
        assert!(err.contains("Todas as tentativas do recovery block falharam"));
    }

    #[test]
    fn acceptance_test_rejeita_resposta_valida() {
        let primary = NamedMock::new("primary", 1.0, Ok("conteúdo rejeitado".into()));

        struct RejectContent;
        impl AcceptanceTest for RejectContent {
            fn evaluate(
                &self,
                response: &ChatResponse,
                _request: &ChatRequest,
            ) -> AcceptanceResult {
                if response.content.contains("rejeitado") {
                    AcceptanceResult::Fail {
                        reason: "contém rejeitado".into(),
                    }
                } else {
                    AcceptanceResult::Pass
                }
            }
        }

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(RejectContent));

        let err = mgr.execute(dummy_request()).unwrap_err().to_string();
        assert!(err.contains("Todas as tentativas do recovery block falharam"));
    }

    #[test]
    fn ollama_provider_custo_zero() {
        let bb = temp_blackboard().unwrap();
        let ollama = OllamaProvider::new(bb);
        assert_eq!(ollama.cost_estimate(1000, 1000), 0.0);
    }

    #[test]
    fn provider_real_cost_estimate_gpt4() {
        let openai = OpenAiCompatProvider::new("api.openai.com", 443, None, true);
        // GPT-4o: $2.50/1M input + $10.00/1M output
        let cost = openai.cost_estimate(1_000_000, 1_000_000);
        assert!((cost - 12.50).abs() < 0.0001);
    }

    #[test]
    fn provider_real_cost_estimate_claude() {
        let anthropic = AnthropicProvider::new("api.anthropic.com", 443, "key", true);
        // Claude 3.5 Sonnet: $3.00/1M input + $15.00/1M output
        let cost = anthropic.cost_estimate(1_000_000, 1_000_000);
        assert!((cost - 18.00).abs() < 0.0001);
    }

    #[test]
    fn provider_real_cost_estimate_google() {
        let google = GoogleProvider::new("key".into(), "gemini-1.5-pro".into());
        // Gemini 1.5 Pro: $3.50/1M input + $10.50/1M output
        let cost = google.cost_estimate(1_000_000, 1_000_000);
        assert!((cost - 14.0).abs() < 0.0001);
    }

    #[test]
    fn cost_optimized_com_aceitacao_falha_no_barato() {
        let primary = NamedMock::new("primary", 10.0, Err("falha"));
        let barato = NamedMock::new("barato", 0.1, Ok("rejeitado".into()));
        let caro = NamedMock::new("caro", 5.0, Ok("aceito".into()));

        struct RejectContent;
        impl AcceptanceTest for RejectContent {
            fn evaluate(
                &self,
                response: &ChatResponse,
                _request: &ChatRequest,
            ) -> AcceptanceResult {
                if response.content.contains("rejeitado") {
                    AcceptanceResult::Fail {
                        reason: "rejeitado".into(),
                    }
                } else {
                    AcceptanceResult::Pass
                }
            }
        }

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(RejectContent))
            .with_failover_strategy(FailoverStrategy::CostOptimized)
            .add_alternate(Box::new(barato))
            .add_alternate(Box::new(caro));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "caro");
        assert_eq!(result.attempts, 3);
    }
}
