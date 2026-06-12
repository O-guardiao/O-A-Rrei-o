use crate::{
    cost_tracker::CostTracker, pool::RoutingStrategy, ClassifiedRequest, ProviderPool,
    RequestType, SensitivityLevel, TaskComplexity,
};

/// Status do budget para uma sessão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetStatus {
    /// Dentro do budget, sem alertas.
    WithinBudget,
    /// Acima do threshold de warning (ex: 80%).
    Warning,
    /// Budget excedido.
    Exceeded,
}

/// Decisão de roteamento determinística.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    /// Prossegue com a estratégia indicada.
    Proceed { strategy: RoutingStrategy },
    /// Fallback explícito para provider mais barato.
    FallbackToCheaper,
    /// Rejeita request porque budget foi excedido.
    RejectBudgetExceeded,
    /// Rejeita porque dados sensíveis e não há provider local disponível.
    RejectSensitiveNoLocalProvider,
}

/// Política de roteamento com awareness de budget, complexidade e sensibilidade.
///
/// Totalmente determinística — nenhuma chamada LLM é feita.
#[derive(Debug, Clone)]
pub struct RoutingPolicy {
    /// Budget máximo em USD por sessão. `None` = sem limite.
    pub budget_max_usd: Option<f64>,
    /// Percentual do budget que dispara warning (default: 80.0).
    pub budget_warn_percent: f64,
    /// Se `true`, prefere provider local (Ollama) para dados `High` sensitivity.
    pub prefer_local_for_sensitive: bool,
    /// Se `true`, rejeita requests quando budget excedido.
    pub enforce_budget: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl RoutingPolicy {
    /// Cria política padrão: sem budget, warn em 80%, não rejeita.
    pub fn new() -> Self {
        Self {
            budget_max_usd: None,
            budget_warn_percent: 80.0,
            prefer_local_for_sensitive: true,
            enforce_budget: false,
        }
    }

    /// Define budget máximo por sessão (ativa enforcement).
    pub fn with_budget(mut self, max_usd: f64) -> Self {
        self.budget_max_usd = Some(max_usd.max(0.0));
        self.enforce_budget = true;
        self
    }

    /// Define threshold de warning (default 80%).
    pub fn with_warn_threshold(mut self, percent: f64) -> Self {
        self.budget_warn_percent = percent.clamp(0.0, 100.0);
        self
    }

    /// Ativa/desativa preferência por provider local para dados sensíveis.
    pub fn with_local_preference(mut self, enabled: bool) -> Self {
        self.prefer_local_for_sensitive = enabled;
        self
    }

    /// Verifica o status do budget para uma sessão específica.
    pub fn check_budget(&self, tracker: &CostTracker, session_id: &str) -> BudgetStatus {
        let Some(max) = self.budget_max_usd else {
            return BudgetStatus::WithinBudget;
        };
        if max <= 0.0 {
            return BudgetStatus::WithinBudget;
        }

        let used = tracker
            .report_by_session(session_id)
            .map(|s| s.total_usd)
            .unwrap_or(0.0);

        let ratio = used / max;
        if ratio >= 1.0 {
            BudgetStatus::Exceeded
        } else if ratio >= (self.budget_warn_percent / 100.0) {
            BudgetStatus::Warning
        } else {
            BudgetStatus::WithinBudget
        }
    }

    /// Toma decisão de roteamento determinística.
    ///
    /// Lógica:
    /// 1. Se budget enforcement ativo e excedido → RejectBudgetExceeded.
    /// 2. Se sensibilidade High e prefer_local ativo mas sem provider local → RejectSensitiveNoLocalProvider.
    /// 3. Complexidade + tipo → estratégia de routing.
    pub fn decide(
        &self,
        pool: &ProviderPool,
        classification: &ClassifiedRequest,
        tracker: &CostTracker,
        session_id: &str,
    ) -> anyhow::Result<RoutingDecision> {
        // 1. Budget gate
        if self.enforce_budget {
            match self.check_budget(tracker, session_id) {
                BudgetStatus::Exceeded => {
                    return Ok(RoutingDecision::RejectBudgetExceeded);
                }
                _ => {}
            }
        }

        // 2. Sensitivity gate
        if self.prefer_local_for_sensitive
            && classification.sensitivity == SensitivityLevel::High
        {
            let names = pool.provider_names();
            let has_local = names.iter().any(|n| {
                let nl = n.to_lowercase();
                nl.contains("ollama") || nl.contains("local")
            });
            if !has_local {
                return Ok(RoutingDecision::RejectSensitiveNoLocalProvider);
            }
        }

        // 3. Complexity-based strategy selection
        let strategy = match (classification.complexity, classification.request_type) {
            (TaskComplexity::Simple, _) => RoutingStrategy::CostOptimized,
            (TaskComplexity::Moderate, RequestType::QuickQuery) => {
                RoutingStrategy::LatencyOptimized
            }
            (TaskComplexity::Moderate, _) => RoutingStrategy::Priority,
            (TaskComplexity::Complex, RequestType::CodeGeneration) => {
                RoutingStrategy::QualityOptimized
            }
            (TaskComplexity::Complex, RequestType::MathTask) => {
                // Math tasks benefit from reasoning models (quality)
                RoutingStrategy::QualityOptimized
            }
            (TaskComplexity::Complex, _) => RoutingStrategy::Priority,
        };

        Ok(RoutingDecision::Proceed { strategy })
    }

    /// Retorna uma string descritiva da decisão para métricas/logs.
    pub fn decision_label(decision: &RoutingDecision) -> &'static str {
        match decision {
            RoutingDecision::Proceed { .. } => "proceed",
            RoutingDecision::FallbackToCheaper => "fallback_cheaper",
            RoutingDecision::RejectBudgetExceeded => "reject_budget",
            RoutingDecision::RejectSensitiveNoLocalProvider => "reject_sensitive",
        }
    }
}

// ===================================================================
// Testes
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider::{ChatRequest, ProviderClient},
        ChatResponse,
    };

    struct FakeProvider {
        name: &'static str,
        cost: f64,
    }

    impl ProviderClient for FakeProvider {
        fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
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
            Box::new(FakeProvider {
                name: self.name,
                cost: self.cost,
            })
        }
        fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
            self.cost
        }
        fn embed(&self, _texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
            Err(anyhow::anyhow!("no"))
        }
        fn chat_stream(
            &self,
            _req: ChatRequest,
        ) -> anyhow::Result<Box<dyn Iterator<Item = anyhow::Result<String>> + Send>> {
            Err(anyhow::anyhow!("no"))
        }
    }

    fn pool_with_providers() -> ProviderPool {
        ProviderPool::new(crate::FailoverStrategy::Priority)
            .add_provider(Box::new(FakeProvider {
                name: "ollama",
                cost: 0.0,
            }))
            .add_provider(Box::new(FakeProvider {
                name: "openai",
                cost: 0.01,
            }))
    }

    fn classified_simple() -> ClassifiedRequest {
        ClassifiedRequest {
            complexity: TaskComplexity::Simple,
            sensitivity: SensitivityLevel::Low,
            request_type: RequestType::QuickQuery,
            estimated_tokens: 100,
            has_code_blocks: false,
            has_math_expressions: false,
        }
    }

    fn classified_complex_code() -> ClassifiedRequest {
        ClassifiedRequest {
            complexity: TaskComplexity::Complex,
            sensitivity: SensitivityLevel::Low,
            request_type: RequestType::CodeGeneration,
            estimated_tokens: 5000,
            has_code_blocks: true,
            has_math_expressions: false,
        }
    }

    fn classified_sensitive() -> ClassifiedRequest {
        ClassifiedRequest {
            complexity: TaskComplexity::Simple,
            sensitivity: SensitivityLevel::High,
            request_type: RequestType::QuickQuery,
            estimated_tokens: 100,
            has_code_blocks: false,
            has_math_expressions: false,
        }
    }

    #[test]
    fn policy_no_budget_always_within() {
        let policy = RoutingPolicy::new();
        let tracker = CostTracker::new();
        assert_eq!(
            policy.check_budget(&tracker, "sess"),
            BudgetStatus::WithinBudget
        );
    }

    #[test]
    fn policy_budget_exceeded() {
        let mut tracker = CostTracker::new();
        tracker.record("sess", "openai", 1000, 500, 5.0);

        let policy = RoutingPolicy::new().with_budget(3.0);
        assert_eq!(
            policy.check_budget(&tracker, "sess"),
            BudgetStatus::Exceeded
        );
    }

    #[test]
    fn policy_budget_warning() {
        let mut tracker = CostTracker::new();
        tracker.record("sess", "openai", 1000, 500, 1.7);

        let policy = RoutingPolicy::new().with_budget(2.0).with_warn_threshold(80.0);
        assert_eq!(
            policy.check_budget(&tracker, "sess"),
            BudgetStatus::Warning
        );
    }

    #[test]
    fn policy_budget_within() {
        let mut tracker = CostTracker::new();
        tracker.record("sess", "openai", 1000, 500, 0.5);

        let policy = RoutingPolicy::new().with_budget(2.0);
        assert_eq!(
            policy.check_budget(&tracker, "sess"),
            BudgetStatus::WithinBudget
        );
    }

    #[test]
    fn policy_decide_simple_uses_cost_optimized() {
        let pool = pool_with_providers();
        let policy = RoutingPolicy::new();
        let tracker = CostTracker::new();
        let cls = classified_simple();

        let dec = policy.decide(&pool, &cls, &tracker, "sess").unwrap();
        assert!(
            matches!(dec, RoutingDecision::Proceed { strategy: RoutingStrategy::CostOptimized }),
            "Simple tasks devem usar CostOptimized, foi {:?}",
            dec
        );
    }

    #[test]
    fn policy_decide_complex_code_uses_quality() {
        let pool = pool_with_providers();
        let policy = RoutingPolicy::new();
        let tracker = CostTracker::new();
        let cls = classified_complex_code();

        let dec = policy.decide(&pool, &cls, &tracker, "sess").unwrap();
        assert!(
            matches!(dec, RoutingDecision::Proceed { strategy: RoutingStrategy::QualityOptimized }),
            "Complex code tasks devem usar QualityOptimized, foi {:?}",
            dec
        );
    }

    #[test]
    fn policy_enforce_budget_rejects() {
        let pool = pool_with_providers();
        let mut tracker = CostTracker::new();
        tracker.record("sess", "openai", 1000, 500, 10.0);

        let policy = RoutingPolicy::new().with_budget(5.0);
        let cls = classified_simple();

        let dec = policy.decide(&pool, &cls, &tracker, "sess").unwrap();
        assert_eq!(dec, RoutingDecision::RejectBudgetExceeded);
    }

    #[test]
    fn policy_sensitive_with_local_allows() {
        let pool = pool_with_providers(); // tem ollama
        let policy = RoutingPolicy::new().with_local_preference(true);
        let tracker = CostTracker::new();
        let cls = classified_sensitive();

        let dec = policy.decide(&pool, &cls, &tracker, "sess").unwrap();
        assert!(
            matches!(dec, RoutingDecision::Proceed { .. }),
            "Deve permitir quando há provider local, foi {:?}",
            dec
        );
    }

    #[test]
    fn policy_sensitive_without_local_rejects() {
        let pool = ProviderPool::new(crate::FailoverStrategy::Priority)
            .add_provider(Box::new(FakeProvider {
                name: "openai",
                cost: 0.01,
            }));
        let policy = RoutingPolicy::new().with_local_preference(true);
        let tracker = CostTracker::new();
        let cls = classified_sensitive();

        let dec = policy.decide(&pool, &cls, &tracker, "sess").unwrap();
        assert_eq!(dec, RoutingDecision::RejectSensitiveNoLocalProvider);
    }

    #[test]
    fn policy_decision_label_coverage() {
        assert_eq!(
            RoutingPolicy::decision_label(&RoutingDecision::Proceed {
                strategy: RoutingStrategy::Priority,
            }),
            "proceed"
        );
        assert_eq!(
            RoutingPolicy::decision_label(&RoutingDecision::RejectBudgetExceeded),
            "reject_budget"
        );
    }
}
