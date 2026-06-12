use crate::pool::FailoverStrategy;
use crate::provider::{ChatRequest, ChatResponse, ProviderClient};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

// ===================================================================
// Acceptance Test
// ===================================================================

/// Resultado da avaliação de um teste de aceitação.
#[derive(Debug)]
pub enum AcceptanceResult {
    Pass,
    Fail { reason: String },
}

/// Trait para testes de aceitação que validam respostas de LLM.
pub trait AcceptanceTest: Send + Sync {
    /// Avalia se uma resposta passa no teste de aceitação.
    fn evaluate(&self, response: &ChatResponse, request: &ChatRequest) -> AcceptanceResult;
}

// ===================================================================
// State Restorer
// ===================================================================

/// Trait para salvamento e restauração de estado entre tentativas.
pub trait StateRestorer: Send + Sync {
    /// Salva um checkpoint antes da execução.
    fn checkpoint(&self) -> Result<String>;
    /// Restaura o estado para o checkpoint informado.
    fn restore(&self, checkpoint_id: &str) -> Result<()>;
}

/// Restaurador de estado que não faz nada (padrão quando não é necessário).
pub struct NoOpStateRestorer;

impl StateRestorer for NoOpStateRestorer {
    fn checkpoint(&self) -> Result<String> {
        Ok("noop".to_string())
    }

    fn restore(&self, _checkpoint_id: &str) -> Result<()> {
        Ok(())
    }
}

/// Restaurador de estado baseado em git stash + checkout.
pub struct GitStateRestorer {
    work_dir: PathBuf,
}

impl GitStateRestorer {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }
}

impl StateRestorer for GitStateRestorer {
    fn checkpoint(&self) -> Result<String> {
        let output = Command::new("git")
            .current_dir(&self.work_dir)
            .args(["stash", "push", "-m", "arreio-recovery-block-checkpoint"])
            .output()
            .context("falha ao executar git stash")?;

        if !output.status.success() {
            anyhow::bail!(
                "git stash falhou: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let stash_list = Command::new("git")
            .current_dir(&self.work_dir)
            .args(["stash", "list", "--format=%H"])
            .output()
            .context("falha ao listar stashes")?;

        let stash_id = String::from_utf8_lossy(&stash_list.stdout)
            .lines()
            .next()
            .unwrap_or("stash@{0}")
            .to_string();

        Ok(stash_id)
    }

    fn restore(&self, checkpoint_id: &str) -> Result<()> {
        let output = Command::new("git")
            .current_dir(&self.work_dir)
            .args(["stash", "pop", checkpoint_id])
            .output()
            .context("falha ao executar git stash pop")?;

        if !output.status.success() {
            // Se pop falhar, tenta aplicar com drop separado
            let apply = Command::new("git")
                .current_dir(&self.work_dir)
                .args(["stash", "apply", checkpoint_id])
                .output()
                .context("falha ao executar git stash apply")?;

            if !apply.status.success() {
                anyhow::bail!(
                    "git stash restore falhou: {}",
                    String::from_utf8_lossy(&apply.stderr)
                );
            }

            let _ = Command::new("git")
                .current_dir(&self.work_dir)
                .args(["stash", "drop", checkpoint_id])
                .output();
        }

        Ok(())
    }
}

// ===================================================================
// Built-in Acceptance Tests
// ===================================================================

/// Valida formato da resposta (JSON schema válido, não vazio, etc.).
pub struct FormatValidationTest;

impl AcceptanceTest for FormatValidationTest {
    fn evaluate(&self, response: &ChatResponse, _request: &ChatRequest) -> AcceptanceResult {
        if response.content.trim().is_empty() {
            return AcceptanceResult::Fail {
                reason: "resposta vazia".into(),
            };
        }

        // Verifica se o conteúdo parece ser JSON válido (quando contém { ou [)
        let trimmed = response.content.trim();
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && serde_json::from_str::<serde_json::Value>(trimmed).is_err()
        {
            return AcceptanceResult::Fail {
                reason: "JSON malformado".into(),
            };
        }

        AcceptanceResult::Pass
    }
}

/// Verifica se a resposta contém campos/padrões obrigatórios.
pub struct ContentPatternTest {
    patterns: Vec<String>,
}

impl ContentPatternTest {
    pub fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }
}

impl AcceptanceTest for ContentPatternTest {
    fn evaluate(&self, response: &ChatResponse, _request: &ChatRequest) -> AcceptanceResult {
        for pattern in &self.patterns {
            if !response.content.contains(pattern) {
                return AcceptanceResult::Fail {
                    reason: format!("padrão '{}' não encontrado", pattern),
                };
            }
        }
        AcceptanceResult::Pass
    }
}

/// Garante que a contagem de tokens esteja dentro de limites razoáveis.
pub struct TokenBoundsTest {
    max_tokens: u64,
}

impl TokenBoundsTest {
    pub fn new(max_tokens: u64) -> Self {
        Self { max_tokens }
    }
}

impl AcceptanceTest for TokenBoundsTest {
    fn evaluate(&self, response: &ChatResponse, _request: &ChatRequest) -> AcceptanceResult {
        if response.tokens_out > self.max_tokens {
            return AcceptanceResult::Fail {
                reason: format!(
                    "tokens {} excedem o limite {}",
                    response.tokens_out, self.max_tokens
                ),
            };
        }
        AcceptanceResult::Pass
    }
}

/// Teste composto: todos os sub-testes devem passar.
pub struct CompositeTest {
    tests: Vec<Box<dyn AcceptanceTest>>,
}

impl CompositeTest {
    pub fn new(tests: Vec<Box<dyn AcceptanceTest>>) -> Self {
        Self { tests }
    }
}

impl AcceptanceTest for CompositeTest {
    fn evaluate(&self, response: &ChatResponse, request: &ChatRequest) -> AcceptanceResult {
        for test in &self.tests {
            match test.evaluate(response, request) {
                AcceptanceResult::Pass => {}
                fail @ AcceptanceResult::Fail { .. } => return fail,
            }
        }
        AcceptanceResult::Pass
    }
}

// ===================================================================
// Recovery Block Result
// ===================================================================

/// Resultado detalhado da execução de um recovery block.
#[derive(Debug)]
pub struct RecoveryBlockResult {
    pub response: ChatResponse,
    pub provider_used: String,
    pub attempts: u32,
    pub acceptance_results: Vec<(String, AcceptanceResult)>,
    pub used_state_restoration: bool,
}

// ===================================================================
// Recovery Block Manager
// ===================================================================

/// Gerenciador de Recovery Block Multi-Model.
/// Executa primary → acceptance test → alternate 1 → ...
pub struct RecoveryBlockManager {
    primary: Box<dyn ProviderClient>,
    alternates: Vec<Box<dyn ProviderClient>>,
    acceptance_test: Box<dyn AcceptanceTest>,
    state_restorer: Box<dyn StateRestorer>,
    max_attempts: u32,
    failover_strategy: FailoverStrategy,
}

impl RecoveryBlockManager {
    pub fn new(primary: Box<dyn ProviderClient>) -> Self {
        Self {
            primary,
            alternates: Vec::new(),
            acceptance_test: Box::new(FormatValidationTest),
            state_restorer: Box::new(NoOpStateRestorer),
            max_attempts: 10,
            failover_strategy: FailoverStrategy::Priority,
        }
    }

    pub fn add_alternate(mut self, provider: Box<dyn ProviderClient>) -> Self {
        self.alternates.push(provider);
        self
    }

    pub fn with_acceptance_test(mut self, test: Box<dyn AcceptanceTest>) -> Self {
        self.acceptance_test = test;
        self
    }

    pub fn with_state_restorer(mut self, restorer: Box<dyn StateRestorer>) -> Self {
        self.state_restorer = restorer;
        self
    }

    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n;
        self
    }

    pub fn with_failover_strategy(mut self, strategy: FailoverStrategy) -> Self {
        self.failover_strategy = strategy;
        self
    }

    /// Executa o recovery block: primary → teste → alternates.
    pub fn execute(&self, request: ChatRequest) -> Result<RecoveryBlockResult> {
        let mut acceptance_results = Vec::new();
        let mut used_state_restoration = false;
        let mut attempts = 0u32;

        // --- Tentativa primária ---
        if attempts < self.max_attempts {
            attempts += 1;
            match self.primary.chat(request.clone()) {
                Ok(response) => {
                    let result = self.acceptance_test.evaluate(&response, &request);
                    let passed = matches!(result, AcceptanceResult::Pass);
                    acceptance_results.push((self.primary.name().to_string(), result));
                    if passed {
                        return Ok(RecoveryBlockResult {
                            response,
                            provider_used: self.primary.name().to_string(),
                            attempts,
                            acceptance_results,
                            used_state_restoration,
                        });
                    }
                }
                Err(e) => {
                    acceptance_results.push((
                        self.primary.name().to_string(),
                        AcceptanceResult::Fail {
                            reason: format!("erro do provider: {}", e),
                        },
                    ));
                }
            }
        }

        // --- Prepara alternates (ordem depende da estratégia) ---
        let alternates: Vec<&Box<dyn ProviderClient>> = match self.failover_strategy {
            FailoverStrategy::Priority | FailoverStrategy::RoundRobin => {
                self.alternates.iter().collect()
            }
            FailoverStrategy::CostOptimized => {
                let mut alts: Vec<_> = self.alternates.iter().collect();
                alts.sort_by(|a, b| {
                    let cost_a = a.cost_estimate(1, 1);
                    let cost_b = b.cost_estimate(1, 1);
                    cost_a
                        .partial_cmp(&cost_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                alts
            }
        };

        // --- Tentativas alternates ---
        for provider in &alternates {
            if attempts >= self.max_attempts {
                break;
            }

            let checkpoint_id = self.state_restorer.checkpoint()?;
            self.state_restorer.restore(&checkpoint_id)?;
            used_state_restoration = true;

            attempts += 1;

            let response = match provider.chat(request.clone()) {
                Ok(r) => r,
                Err(e) => {
                    acceptance_results.push((
                        provider.name().to_string(),
                        AcceptanceResult::Fail {
                            reason: format!("erro do provider: {}", e),
                        },
                    ));
                    continue;
                }
            };

            let result = self.acceptance_test.evaluate(&response, &request);
            let passed = matches!(result, AcceptanceResult::Pass);
            acceptance_results.push((provider.name().to_string(), result));

            if passed {
                return Ok(RecoveryBlockResult {
                    response,
                    provider_used: provider.name().to_string(),
                    attempts,
                    acceptance_results,
                    used_state_restoration,
                });
            }
        }

        anyhow::bail!(
            "Todas as tentativas do recovery block falharam ({} tentativas). Resultados: {:?}",
            attempts,
            acceptance_results
                .iter()
                .map(|(name, res)| match res {
                    AcceptanceResult::Pass => format!("{}: pass", name),
                    AcceptanceResult::Fail { reason } => format!("{}: fail ({})", name, reason),
                })
                .collect::<Vec<_>>()
        )
    }
}

impl ProviderClient for RecoveryBlockManager {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let result = self.execute(req)?;
        Ok(result.response)
    }

    fn name(&self) -> &'static str {
        "RecoveryBlockManager"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        let mut new_mgr = RecoveryBlockManager::new(self.primary.clone_box());
        for alt in &self.alternates {
            new_mgr = new_mgr.add_alternate(alt.clone_box());
        }
        // Não podemos clonar dyn AcceptanceTest / StateRestorer facilmente;
        // usamos os defaults e o caller pode reconfigurar se necessário.
        Box::new(new_mgr)
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        self.primary.cost_estimate(input_tokens, output_tokens)
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.primary.embed(texts)
    }

    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        self.primary.chat_stream(req)
    }
}

// ===================================================================
// Testes
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatRequest, ChatResponse, ProviderClient};
    use crate::MockProvider;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Mock de acceptance test que falha quando o conteúdo contém "fail".
    struct FailOnContentTest {
        forbidden: String,
    }

    impl AcceptanceTest for FailOnContentTest {
        fn evaluate(&self, response: &ChatResponse, _request: &ChatRequest) -> AcceptanceResult {
            if response.content.contains(&self.forbidden) {
                AcceptanceResult::Fail {
                    reason: format!("contém '{}'", self.forbidden),
                }
            } else {
                AcceptanceResult::Pass
            }
        }
    }

    /// Mock de state restorer que rastreia chamadas.
    struct TrackingStateRestorer {
        checkpoints: Arc<AtomicUsize>,
        restores: Arc<AtomicUsize>,
    }

    impl StateRestorer for TrackingStateRestorer {
        fn checkpoint(&self) -> Result<String> {
            self.checkpoints.fetch_add(1, Ordering::SeqCst);
            Ok("ckpt".to_string())
        }

        fn restore(&self, _id: &str) -> Result<()> {
            self.restores.fetch_add(1, Ordering::SeqCst);
            Ok(())
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

    fn dummy_response(content: &str) -> ChatResponse {
        ChatResponse {
            content: content.into(),
            tool_calls: None,
            tokens_in: 1,
            tokens_out: content.split_whitespace().count() as u64,
            rate_limit: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn primary_passes_acceptance_test() {
        let primary = MockProvider::new("primary ok");
        let mgr = RecoveryBlockManager::new(Box::new(primary));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "mock");
        assert_eq!(result.response.content, "primary ok");
        assert_eq!(result.attempts, 1);
        assert!(!result.used_state_restoration);
    }

    #[test]
    fn primary_fails_fallback_to_alternate() {
        let primary = MockProvider::new("fail content");
        let alternate = MockProvider::new("alternate ok");

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(FailOnContentTest {
                forbidden: "fail".into(),
            }))
            .add_alternate(Box::new(alternate));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "mock");
        assert_eq!(result.response.content, "alternate ok");
        assert_eq!(result.attempts, 2);
    }

    #[test]
    fn all_alternates_fail_returns_error() {
        let primary = MockProvider::new("fail primary");
        let alt1 = MockProvider::new("fail alt1");
        let alt2 = MockProvider::new("fail alt2");

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(FailOnContentTest {
                forbidden: "fail".into(),
            }))
            .add_alternate(Box::new(alt1))
            .add_alternate(Box::new(alt2));

        let err = mgr.execute(dummy_request()).unwrap_err().to_string();
        assert!(err.contains("Todas as tentativas do recovery block falharam"));
    }

    #[test]
    fn format_validation_test_rejects_empty() {
        let test = FormatValidationTest;
        let req = dummy_request();
        let mut resp = dummy_response("ok");

        assert!(matches!(test.evaluate(&resp, &req), AcceptanceResult::Pass));

        resp.content = "".into();
        assert!(matches!(
            test.evaluate(&resp, &req),
            AcceptanceResult::Fail { .. }
        ));

        resp.content = "   ".into();
        assert!(matches!(
            test.evaluate(&resp, &req),
            AcceptanceResult::Fail { .. }
        ));
    }

    #[test]
    fn composite_test_all_must_pass() {
        let test = CompositeTest::new(vec![
            Box::new(FormatValidationTest),
            Box::new(ContentPatternTest::new(vec!["required".into()])),
        ]);

        let req = dummy_request();
        let resp = dummy_response("this has required field");
        assert!(matches!(test.evaluate(&resp, &req), AcceptanceResult::Pass));

        let resp2 = dummy_response("missing");
        assert!(matches!(
            test.evaluate(&resp2, &req),
            AcceptanceResult::Fail { .. }
        ));
    }

    #[test]
    fn token_bounds_test_rejects_excessive() {
        let test = TokenBoundsTest::new(5);
        let req = dummy_request();

        let mut resp = dummy_response("a b c");
        resp.tokens_out = 3;
        assert!(matches!(test.evaluate(&resp, &req), AcceptanceResult::Pass));

        resp.tokens_out = 100;
        assert!(matches!(
            test.evaluate(&resp, &req),
            AcceptanceResult::Fail { .. }
        ));
    }

    #[test]
    fn state_restoration_called_between_attempts() {
        let checkpoints = Arc::new(AtomicUsize::new(0));
        let restores = Arc::new(AtomicUsize::new(0));

        let primary = MockProvider::new("fail content");
        let alternate = MockProvider::new("alternate ok");

        let restorer = TrackingStateRestorer {
            checkpoints: Arc::clone(&checkpoints),
            restores: Arc::clone(&restores),
        };

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(FailOnContentTest {
                forbidden: "fail".into(),
            }))
            .with_state_restorer(Box::new(restorer))
            .add_alternate(Box::new(alternate));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.response.content, "alternate ok");
        assert!(result.used_state_restoration);
        assert_eq!(checkpoints.load(Ordering::SeqCst), 1);
        assert_eq!(restores.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn max_attempts_respected() {
        let primary = MockProvider::new("fail 1");
        let alt1 = MockProvider::new("fail 2");
        let alt2 = MockProvider::new("fail 3");
        let alt3 = MockProvider::new("fail 4");

        let mgr = RecoveryBlockManager::new(Box::new(primary))
            .with_acceptance_test(Box::new(FailOnContentTest {
                forbidden: "fail".into(),
            }))
            .add_alternate(Box::new(alt1))
            .add_alternate(Box::new(alt2))
            .add_alternate(Box::new(alt3))
            .with_max_attempts(2);

        let err = mgr.execute(dummy_request()).unwrap_err().to_string();
        assert!(err.contains("Todas as tentativas do recovery block falharam"));
        // Deve parar após 2 tentativas (primary + 1 alternate)
        assert!(err.contains("2 tentativas"));
    }

    #[test]
    fn provider_name_in_result() {
        struct NamedMock {
            name: &'static str,
        }
        impl ProviderClient for NamedMock {
            fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
                Ok(dummy_response("ok"))
            }
            fn name(&self) -> &'static str {
                self.name
            }
            fn clone_box(&self) -> Box<dyn ProviderClient> {
                Box::new(NamedMock { name: self.name })
            }
            fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
                0.0
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

        let mgr = RecoveryBlockManager::new(Box::new(NamedMock { name: "PrimaryGPT" }))
            .add_alternate(Box::new(NamedMock {
                name: "AlternateClaude",
            }));

        let result = mgr.execute(dummy_request()).unwrap();
        assert_eq!(result.provider_used, "PrimaryGPT");
    }

    #[test]
    fn implements_provider_client_trait() {
        let primary = MockProvider::new("hello");
        let alternate = MockProvider::new("world");

        let mgr: Box<dyn ProviderClient> = Box::new(
            RecoveryBlockManager::new(Box::new(primary)).add_alternate(Box::new(alternate)),
        );

        assert_eq!(mgr.name(), "RecoveryBlockManager");

        let resp = mgr.chat(dummy_request()).unwrap();
        assert_eq!(resp.content, "hello");
    }

    #[test]
    fn noop_state_restorer_does_nothing() {
        let restorer = NoOpStateRestorer;
        let id = restorer.checkpoint().unwrap();
        assert_eq!(id, "noop");
        assert!(restorer.restore(&id).is_ok());
    }

    #[test]
    fn format_validation_rejects_bad_json() {
        let test = FormatValidationTest;
        let req = dummy_request();

        let mut resp = dummy_response("{\"valid\": true}");
        assert!(matches!(test.evaluate(&resp, &req), AcceptanceResult::Pass));

        resp.content = "{broken json".into();
        assert!(matches!(
            test.evaluate(&resp, &req),
            AcceptanceResult::Fail { .. }
        ));
    }

    #[test]
    fn content_pattern_test_matches() {
        let test = ContentPatternTest::new(vec!["foo".into(), "bar".into()]);
        let req = dummy_request();

        let resp = dummy_response("hello foo world bar");
        assert!(matches!(test.evaluate(&resp, &req), AcceptanceResult::Pass));

        let resp2 = dummy_response("hello foo world");
        assert!(matches!(
            test.evaluate(&resp2, &req),
            AcceptanceResult::Fail { reason } if reason.contains("bar")
        ));
    }
}
