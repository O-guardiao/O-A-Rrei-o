use crate::{
    ChatRequest, ChatResponse, CircuitBreaker, ClassifiedError, CostTracker, ErrorClassifier,
    ProviderClient, RateLimitGuard, RecoveryAction, RoutingPolicy,
};
use crate::request_classifier::{ClassifiedRequest, RequestClassifier};
use crate::routing_policy::RoutingDecision;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Estratégia de roteamento de requisições para providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingStrategy {
    /// Seleciona o primeiro provider saudável.
    Priority,
    /// Distribui carga round-robin entre providers saudáveis.
    RoundRobin,
    /// Escolhe o provider com menor custo estimado.
    CostOptimized,
    /// Escolhe o provider com menor latência média histórica.
    LatencyOptimized,
    /// Escolhe o provider com maior score de qualidade.
    QualityOptimized,
}

/// Estratégia de failover entre providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverStrategy {
    /// Tenta providers em ordem, sempre começando pelo primeiro saudável.
    Priority,
    /// Distribui carga round-robin entre providers saudáveis.
    RoundRobin,
    /// Ordena providers pelo menor custo estimado.
    CostOptimized,
}

/// Pool de providers LLM com failover, circuit breaker e classificação inteligente de erros.
/// Implementa `ProviderClient`, podendo ser usado como drop-in replacement.
pub struct ProviderPool {
    entries: Vec<PoolEntry>,
    strategy: FailoverStrategy,
    current_index: Mutex<usize>,
    max_retries: u32,
    backoff_base_ms: u64,
    max_compress_retries: u32,
    context_threshold: u64,
    rate_guard: RateLimitGuard,
    latency_history: Mutex<HashMap<String, Vec<u64>>>, // provider -> latências ms
    quality_scores: Mutex<HashMap<String, f64>>,       // provider -> score 0.0-1.0
    cost_tracker: Mutex<CostTracker>,
    session_id: String,
    budget_max_usd: Option<f64>,
}

struct PoolEntry {
    provider: Box<dyn ProviderClient>,
    circuit: CircuitBreaker,
    name: String,
}

impl ProviderPool {
    pub fn new(strategy: FailoverStrategy) -> Self {
        Self {
            entries: Vec::new(),
            strategy,
            current_index: Mutex::new(0),
            max_retries: 2,
            backoff_base_ms: 1000,
            max_compress_retries: 1,
            context_threshold: 8192,
            rate_guard: RateLimitGuard::new(),
            latency_history: Mutex::new(HashMap::new()),
            quality_scores: Mutex::new(HashMap::new()),
            cost_tracker: Mutex::new(CostTracker::new()),
            session_id: "default".to_string(),
            budget_max_usd: None,
        }
    }

    /// Registra uma medição de latência para um provider (em milissegundos).
    pub fn record_latency(&self, provider: &str, latency_ms: u64) {
        let mut hist = self.latency_history.lock().unwrap();
        let vec = hist.entry(provider.to_string()).or_default();
        vec.push(latency_ms);
        // Mantém apenas as últimas 100 amostras para evitar crescimento ilimitado.
        if vec.len() > 100 {
            vec.remove(0);
        }
    }

    /// Define o score de qualidade de um provider (0.0 a 1.0).
    pub fn set_quality_score(&self, provider: &str, score: f64) {
        let mut scores = self.quality_scores.lock().unwrap();
        scores.insert(provider.to_string(), score.clamp(0.0, 1.0));
    }

    /// Retorna os nomes dos providers registrados no pool.
    pub fn provider_names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Seleciona um provider saudável baseado na intenção da tarefa.
    pub fn route_by_intent(&self, task_type: &str) -> Option<&dyn ProviderClient> {
        let strategy = match task_type {
            "code_generation" => RoutingStrategy::QualityOptimized,
            "quick_query" => RoutingStrategy::LatencyOptimized,
            "batch_processing" => RoutingStrategy::CostOptimized,
            _ => RoutingStrategy::Priority,
        };
        self.route(strategy)
    }

    /// Roteia baseado em classificação determinística + política de budget/sensibilidade.
    ///
    /// Retorna `None` se a política rejeitar o request (budget excedido ou sensível sem local).
    pub fn route_classified(
        &self,
        classification: &ClassifiedRequest,
        policy: &RoutingPolicy,
    ) -> Option<&dyn ProviderClient> {
        let tracker = self.cost_tracker.lock().unwrap();
        let decision = policy
            .decide(self, classification, &tracker, &self.session_id)
            .ok()?;
        match decision {
            RoutingDecision::Proceed { strategy } => self.route(strategy),
            RoutingDecision::FallbackToCheaper => self.route(RoutingStrategy::CostOptimized),
            RoutingDecision::RejectBudgetExceeded | RoutingDecision::RejectSensitiveNoLocalProvider => None,
        }
    }

    /// Classifica o request automaticamente e roteia via política.
    pub fn route_with_policy(&self, req: &ChatRequest, policy: &RoutingPolicy) -> Option<&dyn ProviderClient> {
        let classifier = RequestClassifier::new();
        let classification = classifier.classify(req);
        self.route_classified(&classification, policy)
    }

    /// Configura o cost tracker interno para tracking automático.
    pub fn with_cost_tracker(mut self, tracker: CostTracker) -> Self {
        self.cost_tracker = Mutex::new(tracker);
        self
    }

    /// Define o ID de sessão para tracking de custo.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = id.into();
        self
    }

    /// Define budget máximo em USD para esta sessão.
    pub fn with_budget(mut self, max_usd: f64) -> Self {
        self.budget_max_usd = Some(max_usd.max(0.0));
        self
    }

    /// Retorna o cost tracker interno (clone).
    pub fn cost_tracker_snapshot(&self) -> CostTracker {
        self.cost_tracker.lock().unwrap().clone()
    }

    /// Verifica se o budget está excedido (se configurado).
    pub fn is_budget_exceeded(&self) -> bool {
        let max = match self.budget_max_usd {
            Some(v) if v > 0.0 => v,
            _ => return false,
        };
        let tracker = self.cost_tracker.lock().unwrap();
        let used = tracker
            .report_by_session(&self.session_id)
            .map(|s| s.total_usd)
            .unwrap_or(0.0);
        used >= max
    }

    fn route(&self, strategy: RoutingStrategy) -> Option<&dyn ProviderClient> {
        let healthy = self.healthy_entries();
        if healthy.is_empty() {
            return None;
        }
        match strategy {
            RoutingStrategy::Priority => Some(healthy[0].provider.as_ref()),
            RoutingStrategy::RoundRobin => {
                let idx = self.next_index() % healthy.len();
                Some(healthy[idx].provider.as_ref())
            }
            RoutingStrategy::CostOptimized => healthy
                .iter()
                .min_by(|a, b| {
                    let cost_a = a.provider.cost_estimate(1, 1);
                    let cost_b = b.provider.cost_estimate(1, 1);
                    cost_a
                        .partial_cmp(&cost_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|e| e.provider.as_ref()),
            RoutingStrategy::LatencyOptimized => {
                let latencies = self.latency_history.lock().unwrap();
                healthy
                    .iter()
                    .min_by(|a, b| {
                        let avg_a = latencies
                            .get(&a.name)
                            .map(|v| {
                                if v.is_empty() {
                                    u64::MAX
                                } else {
                                    v.iter().sum::<u64>() / v.len() as u64
                                }
                            })
                            .unwrap_or(u64::MAX);
                        let avg_b = latencies
                            .get(&b.name)
                            .map(|v| {
                                if v.is_empty() {
                                    u64::MAX
                                } else {
                                    v.iter().sum::<u64>() / v.len() as u64
                                }
                            })
                            .unwrap_or(u64::MAX);
                        avg_a.cmp(&avg_b)
                    })
                    .map(|e| e.provider.as_ref())
            }
            RoutingStrategy::QualityOptimized => {
                let scores = self.quality_scores.lock().unwrap();
                healthy
                    .iter()
                    .max_by(|a, b| {
                        let score_a = scores.get(&a.name).copied().unwrap_or(0.0);
                        let score_b = scores.get(&b.name).copied().unwrap_or(0.0);
                        score_a
                            .partial_cmp(&score_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|e| e.provider.as_ref())
            }
        }
    }

    /// Define o número máximo de retries por provider para erros `Retryable`.
    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Define o backoff base em milissegundos (backoff exponencial: base * 2^retry).
    pub fn with_backoff_base_ms(mut self, ms: u64) -> Self {
        self.backoff_base_ms = ms;
        self
    }

    /// Define o limite de contexto usado para classificar `ContextOverflow`.
    pub fn with_context_threshold(mut self, threshold: u64) -> Self {
        self.context_threshold = threshold;
        self
    }

    pub fn add_provider(mut self, provider: Box<dyn ProviderClient>) -> Self {
        let name = provider.name().to_string();
        self.entries.push(PoolEntry {
            provider,
            circuit: CircuitBreaker::new(3, 60),
            name,
        });
        self
    }

    fn next_index(&self) -> usize {
        let mut idx = self.current_index.lock().unwrap();
        let val = *idx;
        *idx = (*idx + 1) % self.entries.len().max(1);
        val
    }

    fn healthy_entries(&self) -> Vec<&PoolEntry> {
        self.entries
            .iter()
            .filter(|e| e.circuit.current_state() != crate::CircuitState::Open)
            .collect()
    }

    /// Tenta executar uma requisição em um entry, aplicando classificação de erro e ações.
    fn try_entry(
        &self,
        entry: &PoolEntry,
        req: &ChatRequest,
        compressed: bool,
        retry_count: u32,
    ) -> Result<ChatResponse, (anyhow::Error, ClassifiedError)> {
        if let Err(e) = self.rate_guard.pre_flight_check(&entry.name) {
            let err = anyhow::anyhow!("{}", e);
            let classified = ErrorClassifier::classify_with_context(
                &err,
                estimate_tokens(req),
                self.context_threshold,
            );
            return Err((err, classified));
        }

        match entry
            .circuit
            .call_with_error(|| entry.provider.chat(req.clone()))
        {
            Ok(resp) => {
                self.rate_guard
                    .record_success(&entry.name, resp.rate_limit.as_ref());
                // Track cost automatically if cost_tracker is configured
                let cost = entry.provider.cost_estimate(resp.tokens_in as u32, resp.tokens_out as u32);
                if let Ok(mut tracker) = self.cost_tracker.lock() {
                    tracker.record(
                        &self.session_id,
                        entry.provider.name(),
                        resp.tokens_in as u32,
                        resp.tokens_out as u32,
                        cost,
                    );
                }
                Ok(resp)
            }
            Err((_, e)) => {
                self.rate_guard.record_error(&entry.name, &e);
                let tokens_in = estimate_tokens(req);
                let classified =
                    ErrorClassifier::classify_with_context(&e, tokens_in, self.context_threshold);

                match classified.action {
                    RecoveryAction::Abort => Err((e, classified)),
                    RecoveryAction::ShouldCompress => {
                        if !compressed && retry_count < self.max_compress_retries {
                            let mut compressed_req = req.clone();
                            compress_request(&mut compressed_req);
                            self.try_entry(entry, &compressed_req, true, retry_count + 1)
                        } else {
                            Err((e, classified))
                        }
                    }
                    RecoveryAction::Retryable => {
                        if retry_count < self.max_retries {
                            let backoff = self.backoff_base_ms * (1_u64 << retry_count);
                            thread::sleep(Duration::from_millis(backoff));
                            self.try_entry(entry, req, compressed, retry_count + 1)
                        } else {
                            Err((e, classified))
                        }
                    }
                    RecoveryAction::ShouldRotateCredential | RecoveryAction::ShouldFallback => {
                        Err((e, classified))
                    }
                }
            }
        }
    }
}

impl ProviderClient for ProviderPool {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        if self.entries.is_empty() {
            anyhow::bail!("ProviderPool vazio — nenhum provider configurado");
        }

        let mut last_error = None;

        match self.strategy {
            FailoverStrategy::Priority | FailoverStrategy::CostOptimized => {
                for entry in &self.entries {
                    match self.try_entry(entry, &req, false, 0) {
                        Ok(resp) => return Ok(resp),
                        Err((e, classified)) => {
                            last_error =
                                Some(format!("{}: {} ({})", entry.name, e, classified.reason));
                            if classified.action == RecoveryAction::Abort {
                                break;
                            }
                        }
                    }
                }
            }
            FailoverStrategy::RoundRobin => {
                let healthy = self.healthy_entries();
                if healthy.is_empty() {
                    anyhow::bail!("Todos os providers estão com circuit breaker aberto");
                }
                let start = self.next_index() % healthy.len();
                for i in 0..healthy.len() {
                    let entry = healthy[(start + i) % healthy.len()];
                    match self.try_entry(entry, &req, false, 0) {
                        Ok(resp) => return Ok(resp),
                        Err((e, classified)) => {
                            last_error =
                                Some(format!("{}: {} ({})", entry.name, e, classified.reason));
                            if classified.action == RecoveryAction::Abort {
                                break;
                            }
                        }
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "Todos os providers falharam. Último erro: {}",
            last_error.unwrap_or_else(|| "desconhecido".into())
        ))
    }

    fn name(&self) -> &'static str {
        "ProviderPool"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        let mut new_pool = ProviderPool::new(self.strategy);
        for entry in &self.entries {
            new_pool = new_pool.add_provider(entry.provider.clone_box());
        }
        new_pool.rate_guard = self.rate_guard.clone();
        // Copia histórico de latência e scores de qualidade.
        *new_pool.latency_history.lock().unwrap() = self.latency_history.lock().unwrap().clone();
        *new_pool.quality_scores.lock().unwrap() = self.quality_scores.lock().unwrap().clone();
        *new_pool.cost_tracker.lock().unwrap() = self.cost_tracker.lock().unwrap().clone();
        new_pool.session_id = self.session_id.clone();
        new_pool.budget_max_usd = self.budget_max_usd;
        Box::new(new_pool)
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        // Delega ao primeiro provider configurado; retorna 0.0 se o pool estiver vazio.
        self.entries
            .first()
            .map(|e| e.provider.cost_estimate(input_tokens, output_tokens))
            .unwrap_or(0.0)
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        // Tenta embed no primeiro provider saudável.
        for entry in &self.entries {
            if entry.circuit.current_state() != crate::CircuitState::Open {
                return entry.provider.embed(texts);
            }
        }
        anyhow::bail!("ProviderPool vazio — nenhum provider configurado para embed")
    }

    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        // Delega ao primeiro provider saudável.
        // Fallback em stream não é suportado porque trocar de provider no meio
        // do stream quebraria a experiência do cliente (chunks parciais).
        for entry in &self.entries {
            if entry.circuit.current_state() != crate::CircuitState::Open {
                return entry.provider.chat_stream(req);
            }
        }
        anyhow::bail!("ProviderPool vazio — nenhum provider configurado para streaming")
    }
}

// ------------------------------------------------------------------
// Utilitários
// ------------------------------------------------------------------

fn estimate_tokens(req: &ChatRequest) -> u64 {
    // Estimativa rápida: ~4 chars por token
    let text_len = req.system.len() + req.user.len();
    (text_len / 4) as u64
}

fn compress_request(req: &mut ChatRequest) {
    fn truncate(s: &mut String, max_len: usize) {
        if s.len() > max_len && max_len > 20 {
            let keep = max_len / 2;
            let prefix = s[..keep].to_string();
            let suffix = s[s.len() - keep..].to_string();
            *s = format!("{}...(truncado)...{}", prefix, suffix);
        }
    }

    let sys_max = (req.system.len() / 2).max(100);
    let user_max = (req.user.len() / 2).max(100);
    truncate(&mut req.system, sys_max);
    truncate(&mut req.user, user_max);
}

// ===================================================================
// Testes
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatRequest, ChatResponse, ProviderClient};

    /// Mock que retorna erros determinísticos baseados no conteúdo do prompt.
    struct ErrorMock {
        name: &'static str,
        error_msg: String,
    }

    impl ErrorMock {
        fn new(name: &'static str, error_msg: impl Into<String>) -> Self {
            Self {
                name,
                error_msg: error_msg.into(),
            }
        }
    }

    impl ProviderClient for ErrorMock {
        fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
            Err(anyhow::anyhow!("{}", self.error_msg))
        }
        fn name(&self) -> &'static str {
            self.name
        }
        fn clone_box(&self) -> Box<dyn ProviderClient> {
            Box::new(ErrorMock {
                name: self.name,
                error_msg: self.error_msg.clone(),
            })
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

    /// Mock simples com nome customizável para testes de roteamento.
    struct NamedMock {
        name: &'static str,
        cost: f64,
    }

    impl NamedMock {
        fn new(name: &'static str) -> Self {
            Self { name, cost: 0.0 }
        }
        #[allow(dead_code)]
        fn with_cost(name: &'static str, cost: f64) -> Self {
            Self { name, cost }
        }
    }

    impl ProviderClient for NamedMock {
        fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                content: "ok".into(),
                tool_calls: None,
                tokens_in: 1,
                tokens_out: 1,
                rate_limit: None,
                reasoning_content: None,
            })
        }
        fn name(&self) -> &'static str {
            self.name
        }
        fn clone_box(&self) -> Box<dyn ProviderClient> {
            Box::new(NamedMock {
                name: self.name,
                cost: self.cost,
            })
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

    #[test]
    fn pool_priority_fallback_after_retryable() {
        let failing = ErrorMock::new("fail", "HTTP 429 rate limit exceeded");
        let working = crate::MockProvider::new("hello");

        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .with_max_retries(0) // sem retry no mesmo provider
            .add_provider(Box::new(failing))
            .add_provider(Box::new(working));

        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "".into(),
            user: "hi".into(),
            tools: None,
        };

        let resp = pool.chat(req).unwrap();
        assert_eq!(resp.content, "hello");
    }

    #[test]
    fn pool_round_robin_fallback() {
        let failing = ErrorMock::new("fail", "HTTP 502 overloaded");
        let working = crate::MockProvider::new("world");

        let pool = ProviderPool::new(FailoverStrategy::RoundRobin)
            .with_max_retries(0)
            .add_provider(Box::new(failing))
            .add_provider(Box::new(working));

        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "".into(),
            user: "hi".into(),
            tools: None,
        };

        let resp = pool.chat(req).unwrap();
        assert_eq!(resp.content, "world");
    }

    #[test]
    fn pool_abort_imediato() {
        let failing = ErrorMock::new("fail", "content_policy_violation");
        let working = crate::MockProvider::new("never");

        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(failing))
            .add_provider(Box::new(working));

        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "".into(),
            user: "hi".into(),
            tools: None,
        };

        let err = pool.chat(req).unwrap_err().to_string();
        assert!(err.contains("Todos os providers falharam"));
        // Como o erro é Abort, o provider "never" não deve ser chamado
        // (o teste passa se não panica e retorna erro)
    }

    #[test]
    fn pool_compress_retry() {
        let large_prompt = "a".repeat(4000);
        let failing = ErrorMock::new("fail", "context_length_exceeded");
        let working = crate::MockProvider::new("compressed_ok");

        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .with_context_threshold(8000)
            .with_max_retries(0)
            .add_provider(Box::new(failing))
            .add_provider(Box::new(working));

        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: large_prompt.clone(),
            user: large_prompt,
            tools: None,
        };

        // O primeiro provider falha com context overflow → comprime e retry nele
        // mas como o mock sempre falha, vai para o segundo
        let resp = pool.chat(req).unwrap();
        assert_eq!(resp.content, "compressed_ok");
    }

    #[test]
    fn pool_retryable_backoff() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct RetryMock {
            calls: Arc<AtomicUsize>,
        }
        impl ProviderClient for RetryMock {
            fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(anyhow::anyhow!("HTTP 500 server error"))
                } else {
                    Ok(ChatResponse {
                        content: "recovered".into(),
                        tool_calls: None,
                        tokens_in: 1,
                        tokens_out: 1,
                        rate_limit: None,
                        reasoning_content: None,
                    })
                }
            }
            fn name(&self) -> &'static str {
                "retry"
            }
            fn clone_box(&self) -> Box<dyn ProviderClient> {
                Box::new(RetryMock {
                    calls: Arc::clone(&self.calls),
                })
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

        let calls = Arc::new(AtomicUsize::new(0));
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .with_max_retries(2)
            .with_backoff_base_ms(10)
            .add_provider(Box::new(RetryMock {
                calls: Arc::clone(&calls),
            }));

        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "".into(),
            user: "hi".into(),
            tools: None,
        };

        let resp = pool.chat(req).unwrap();
        assert_eq!(resp.content, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn pool_empty_fails() {
        let pool = ProviderPool::new(FailoverStrategy::Priority);
        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "".into(),
            user: "hi".into(),
            tools: None,
        };
        assert!(pool.chat(req).is_err());
    }

    #[test]
    fn pool_compress_truncates_text() {
        let mut req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "s".repeat(200),
            user: "u".repeat(200),
            tools: None,
        };
        compress_request(&mut req);
        assert!(req.system.len() < 200);
        assert!(req.user.len() < 200);
        assert!(req.system.contains("truncado"));
        assert!(req.user.contains("truncado"));
    }

    #[test]
    fn pool_all_fail_classified() {
        let p1 = ErrorMock::new("p1", "billing_hard_limit_reached");
        let p2 = ErrorMock::new("p2", "invalid_api_key");

        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .with_max_retries(0)
            .add_provider(Box::new(p1))
            .add_provider(Box::new(p2));

        let req = ChatRequest {
            messages: Vec::new(),
            model: "test".into(),
            system: "".into(),
            user: "hi".into(),
            tools: None,
        };

        let err = pool.chat(req).unwrap_err().to_string();
        assert!(err.contains("Billing") || err.contains("AuthPermanent"));
    }

    // ------------------------------------------------------------------
    // Testes de roteamento por intenção
    // ------------------------------------------------------------------

    #[test]
    fn route_by_intent_code_generation_usa_quality() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("a")))
            .add_provider(Box::new(NamedMock::new("b")));

        let prov = pool.route_by_intent("code_generation");
        assert!(prov.is_some());
    }

    #[test]
    fn route_by_intent_quick_query_usa_latency() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("a")));

        let prov = pool.route_by_intent("quick_query");
        assert!(prov.is_some());
        assert_eq!(prov.unwrap().name(), "a");
    }

    #[test]
    fn route_by_intent_batch_processing_usa_cost() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("a")))
            .add_provider(Box::new(NamedMock::new("b")));

        let prov = pool.route_by_intent("batch_processing");
        assert!(prov.is_some());
    }

    #[test]
    fn route_by_intent_default_usa_priority() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("primeiro")))
            .add_provider(Box::new(NamedMock::new("segundo")));

        let prov = pool.route_by_intent("unknown_task");
        assert_eq!(prov.unwrap().name(), "primeiro");
    }

    #[test]
    fn route_by_intent_pool_vazio_retorna_none() {
        let pool = ProviderPool::new(FailoverStrategy::Priority);
        assert!(pool.route_by_intent("code_generation").is_none());
    }

    #[test]
    fn route_by_intent_quality_seleciona_maior_score() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("low")))
            .add_provider(Box::new(NamedMock::new("high")));

        pool.set_quality_score("low", 0.3);
        pool.set_quality_score("high", 0.9);

        let prov = pool.route_by_intent("code_generation");
        assert_eq!(prov.unwrap().name(), "high");
    }

    #[test]
    fn route_by_intent_latency_seleciona_menor_media() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("lento")))
            .add_provider(Box::new(NamedMock::new("rapido")));

        pool.record_latency("lento", 500);
        pool.record_latency("lento", 600);
        pool.record_latency("rapido", 50);
        pool.record_latency("rapido", 70);

        let prov = pool.route_by_intent("quick_query");
        assert_eq!(prov.unwrap().name(), "rapido");
    }

    #[test]
    fn route_by_intent_cost_seleciona_menor_custo() {
        struct CheapMock;
        impl ProviderClient for CheapMock {
            fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
                unreachable!()
            }
            fn name(&self) -> &'static str {
                "cheap"
            }
            fn clone_box(&self) -> Box<dyn ProviderClient> {
                Box::new(CheapMock)
            }
            fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
                0.001
            }
            fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
                unreachable!()
            }
            fn chat_stream(
                &self,
                _req: ChatRequest,
            ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
                unreachable!()
            }
        }
        struct ExpensiveMock;
        impl ProviderClient for ExpensiveMock {
            fn chat(&self, _req: ChatRequest) -> Result<ChatResponse> {
                unreachable!()
            }
            fn name(&self) -> &'static str {
                "expensive"
            }
            fn clone_box(&self) -> Box<dyn ProviderClient> {
                Box::new(ExpensiveMock)
            }
            fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
                0.1
            }
            fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
                unreachable!()
            }
            fn chat_stream(
                &self,
                _req: ChatRequest,
            ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
                unreachable!()
            }
        }

        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(ExpensiveMock))
            .add_provider(Box::new(CheapMock));

        let prov = pool.route_by_intent("batch_processing");
        assert_eq!(prov.unwrap().name(), "cheap");
    }

    #[test]
    fn record_latency_mantem_limite_maximo() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("p")));

        for i in 0..110 {
            pool.record_latency("p", i as u64);
        }

        let hist = pool.latency_history.lock().unwrap();
        let vec = hist.get("p").unwrap();
        assert_eq!(vec.len(), 100);
        // As primeiras 10 amostras devem ter sido removidas.
        assert_eq!(vec[0], 10);
    }

    #[test]
    fn set_quality_score_clampa_valores() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("p")));

        pool.set_quality_score("p", 1.5);
        pool.set_quality_score("p2", -0.5);

        let scores = pool.quality_scores.lock().unwrap();
        assert_eq!(scores.get("p"), Some(&1.0));
        assert_eq!(scores.get("p2"), Some(&0.0));
    }

    #[test]
    fn routing_strategy_variants_existem() {
        let _ = RoutingStrategy::Priority;
        let _ = RoutingStrategy::RoundRobin;
        let _ = RoutingStrategy::CostOptimized;
        let _ = RoutingStrategy::LatencyOptimized;
        let _ = RoutingStrategy::QualityOptimized;
    }

    #[test]
    fn latency_sem_historico_usa_maximo() {
        let pool = ProviderPool::new(FailoverStrategy::Priority)
            .add_provider(Box::new(NamedMock::new("sem_hist")))
            .add_provider(Box::new(NamedMock::new("com_hist")));

        pool.record_latency("com_hist", 10);

        // Sem histórico, "sem_hist" deve ter avg = u64::MAX, então "com_hist" é escolhido.
        let prov = pool.route_by_intent("quick_query");
        assert_eq!(prov.unwrap().name(), "com_hist");
    }
}
