use std::collections::HashMap;

/// Custo acumulado de uma sessão de uso de LLM.
#[derive(Debug, Clone, Default)]
pub struct SessionCost {
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_usd: f64,
    pub request_count: u32,
}

/// Relatório consolidado de custos entre todas as sessões.
#[derive(Debug, Clone)]
pub struct CostReport {
    pub total_usd: f64,
    pub by_provider: HashMap<String, f64>,
    pub by_session: HashMap<String, f64>,
}

/// Rastreador de custos de múltiplas sessões de providers LLM.
#[derive(Debug, Clone)]
pub struct CostTracker {
    sessions: HashMap<String, SessionCost>,
}

impl CostTracker {
    /// Cria um tracker vazio.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Registra uma requisição em uma sessão, acumulando tokens e custo.
    pub fn record(
        &mut self,
        session: &str,
        provider: &str,
        input_tokens: u32,
        output_tokens: u32,
        cost_usd: f64,
    ) {
        let entry = self
            .sessions
            .entry(session.to_string())
            .or_insert(SessionCost {
                provider: provider.to_string(),
                ..SessionCost::default()
            });

        // Se o provider mudou, atualiza para o mais recente.
        entry.provider = provider.to_string();
        entry.input_tokens += input_tokens as u64;
        entry.output_tokens += output_tokens as u64;
        entry.total_usd += cost_usd;
        entry.request_count += 1;
    }

    /// Gera um relatório consolidado de todos os custos registrados.
    pub fn report(&self) -> CostReport {
        let mut total_usd = 0.0;
        let mut by_provider: HashMap<String, f64> = HashMap::new();
        let mut by_session: HashMap<String, f64> = HashMap::new();

        for (session_id, cost) in &self.sessions {
            total_usd += cost.total_usd;
            *by_provider.entry(cost.provider.clone()).or_insert(0.0) += cost.total_usd;
            by_session.insert(session_id.clone(), cost.total_usd);
        }

        CostReport {
            total_usd,
            by_provider,
            by_session,
        }
    }

    /// Retorna o custo detalhado de uma sessão específica, se existir.
    pub fn report_by_session(&self, session: &str) -> Option<SessionCost> {
        self.sessions.get(session).cloned()
    }
}

// ===================================================================
// Testes
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_novo_esta_vazio() {
        let tracker = CostTracker::new();
        let report = tracker.report();
        assert_eq!(report.total_usd, 0.0);
        assert!(report.by_provider.is_empty());
        assert!(report.by_session.is_empty());
    }

    #[test]
    fn record_uma_unica_requisicao() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_a", "ollama", 100, 50, 0.001);

        let report = tracker.report();
        assert_eq!(report.total_usd, 0.001);
        assert_eq!(report.by_provider.get("ollama"), Some(&0.001));
        assert_eq!(report.by_session.get("sessao_a"), Some(&0.001));
    }

    #[test]
    fn record_acumula_tokens_e_custo_na_mesma_sessao() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_a", "openai", 100, 50, 0.001);
        tracker.record("sessao_a", "openai", 200, 100, 0.002);

        let sessao = tracker.report_by_session("sessao_a").unwrap();
        assert_eq!(sessao.input_tokens, 300);
        assert_eq!(sessao.output_tokens, 150);
        assert_eq!(sessao.total_usd, 0.003);
        assert_eq!(sessao.request_count, 2);
    }

    #[test]
    fn report_com_multiplas_sessoes() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_a", "ollama", 100, 50, 0.001);
        tracker.record("sessao_b", "openai", 200, 100, 0.005);

        let report = tracker.report();
        assert_eq!(report.total_usd, 0.006);
        assert_eq!(report.by_session.len(), 2);
        assert_eq!(report.by_session.get("sessao_a"), Some(&0.001));
        assert_eq!(report.by_session.get("sessao_b"), Some(&0.005));
    }

    #[test]
    fn report_by_provider_agrupa_por_provider() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_a", "openai", 100, 50, 0.002);
        tracker.record("sessao_b", "openai", 100, 50, 0.003);
        tracker.record("sessao_c", "anthropic", 100, 50, 0.004);

        let report = tracker.report();
        assert_eq!(report.by_provider.get("openai"), Some(&0.005));
        assert_eq!(report.by_provider.get("anthropic"), Some(&0.004));
    }

    #[test]
    fn report_by_session_existente_retorna_some() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_x", "google", 50, 25, 0.0005);

        let opt = tracker.report_by_session("sessao_x");
        assert!(opt.is_some());
        let cost = opt.unwrap();
        assert_eq!(cost.provider, "google");
        assert_eq!(cost.input_tokens, 50);
    }

    #[test]
    fn report_by_session_inexistente_retorna_none() {
        let tracker = CostTracker::new();
        assert!(tracker.report_by_session("inexistente").is_none());
    }

    #[test]
    fn session_cost_default_todos_zeros() {
        let cost = SessionCost::default();
        assert_eq!(cost.provider, "");
        assert_eq!(cost.input_tokens, 0);
        assert_eq!(cost.output_tokens, 0);
        assert_eq!(cost.total_usd, 0.0);
        assert_eq!(cost.request_count, 0);
    }

    #[test]
    fn session_cost_clone_preserva_valores() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_clone", "azure", 10, 5, 0.0001);

        let original = tracker.report_by_session("sessao_clone").unwrap();
        let cloned = original.clone();
        assert_eq!(cloned.provider, original.provider);
        assert_eq!(cloned.input_tokens, original.input_tokens);
        assert_eq!(cloned.total_usd, original.total_usd);
    }

    #[test]
    fn record_atualiza_provider_se_mudar() {
        let mut tracker = CostTracker::new();
        tracker.record("sessao_a", "ollama", 100, 50, 0.001);
        tracker.record("sessao_a", "openai", 100, 50, 0.002);

        let sessao = tracker.report_by_session("sessao_a").unwrap();
        // O provider deve refletir o último registro.
        assert_eq!(sessao.provider, "openai");
        // O custo total ainda é a soma.
        assert_eq!(sessao.total_usd, 0.003);
    }

    #[test]
    fn report_vazio_nao_panica() {
        let tracker = CostTracker::new();
        let _report = tracker.report();
        // Se chegou aqui, não houve pânica.
    }
}
