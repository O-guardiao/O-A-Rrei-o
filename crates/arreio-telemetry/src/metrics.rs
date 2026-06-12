use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Tipo de métrica.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricType {
    Counter,
    Gauge,
    Histogram,
}

/// Ponto de métrica.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub timestamp: u64,
    pub name: String,
    pub metric_type: MetricType,
    pub value: f64,
    pub labels: Vec<(String, String)>,
}

/// Coletor de métricas persistido no Blackboard.
pub struct MetricsCollector {
    blackboard: Blackboard,
}

impl MetricsCollector {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn record(
        &self,
        name: &str,
        metric_type: MetricType,
        value: f64,
        labels: &[(&str, &str)],
    ) -> anyhow::Result<()> {
        let point = MetricPoint {
            timestamp: now(),
            name: name.into(),
            metric_type,
            value,
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        let key = format!("{}-{}", point.timestamp, uuid::Uuid::new_v4());
        self.blackboard
            .put_tuple("metrics", &key, serde_json::to_value(point)?)
    }

    pub fn count(&self, name: &str, labels: &[(&str, &str)]) -> anyhow::Result<()> {
        self.record(name, MetricType::Counter, 1.0, labels)
    }

    pub fn gauge(&self, name: &str, value: f64, labels: &[(&str, &str)]) -> anyhow::Result<()> {
        self.record(name, MetricType::Gauge, value, labels)
    }

    pub fn query(&self, name_prefix: &str, limit: usize) -> Vec<MetricPoint> {
        let all = self.blackboard.search_tuples("metrics", "");
        let mut points: Vec<MetricPoint> = all
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .filter(|p: &MetricPoint| p.name.starts_with(name_prefix))
            .collect();
        points.sort_by_key(|p| std::cmp::Reverse(p.timestamp));
        points.truncate(limit);
        points
    }

    pub fn latency_percentile(&self, name: &str, p: f64, window_secs: u64) -> Option<f64> {
        let cutoff = now().saturating_sub(window_secs);
        let mut values: Vec<f64> = self
            .query(name, 10000)
            .into_iter()
            .filter(|pt| pt.timestamp >= cutoff && pt.metric_type == MetricType::Histogram)
            .map(|pt| pt.value)
            .collect();
        if values.is_empty() {
            return None;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((values.len() as f64 - 1.0) * p / 100.0) as usize;
        Some(values[idx.min(values.len() - 1)])
    }

    /// Registra o resultado de uma verificação de contrato (PVC-Q1.1).
    /// Atualiza counters de execução e violação, e calcula adherence rate.
    pub fn record_contract_check(&self, violations: usize) -> anyhow::Result<()> {
        self.count("contract_execution_total", &[])?;
        if violations > 0 {
            for _ in 0..violations {
                self.count("contract_violation_total", &[])?;
            }
        }
        // Calcular adherence rate a partir dos counters acumulados
        let executions = self.query("contract_execution_total", 10000).len() as f64;
        let total_violations = self.query("contract_violation_total", 10000).len() as f64;
        let rate = if executions > 0.0 {
            ((executions - total_violations) / executions).clamp(0.0, 1.0)
        } else {
            1.0
        };
        // Sobrescreve o gauge de adherence rate (não acumula) usando chave fixa
        let point = MetricPoint {
            timestamp: now(),
            name: "contract_adherence_rate".into(),
            metric_type: MetricType::Gauge,
            value: rate,
            labels: vec![],
        };
        self.blackboard.put_tuple("metrics", "contract_adherence_rate", serde_json::to_value(point)?)?;
        Ok(())
    }

    // ── Métricas HITL (PVC-Q1.2 + Q1.3) ───────────────────────────────────────

    /// Registra uma decisão HITL (approve/reject/escalate).
    /// Opcionalmente inclui `trace_id` para correlacionar com distributed tracing (PVC-Q1.3).
    pub fn record_hitl_decision(&self, decision: &str, trace_id: Option<&str>) -> anyhow::Result<()> {
        let mut labels = vec![("decision", decision)];
        if let Some(tid) = trace_id {
            labels.push(("trace_id", tid));
        }
        self.count("hitl_decision_total", &labels)
    }

    /// Atualiza o gauge de aprovações pendentes.
    pub fn record_hitl_pending(&self, count: usize) -> anyhow::Result<()> {
        let point = MetricPoint {
            timestamp: now(),
            name: "hitl_pending_gauge".into(),
            metric_type: MetricType::Gauge,
            value: count as f64,
            labels: vec![],
        };
        self.blackboard.put_tuple("metrics", "hitl_pending_gauge", serde_json::to_value(point)?)?;
        Ok(())
    }

    /// Registra duração de uma aprovação em milissegundos.
    pub fn record_hitl_approval_duration_ms(&self, duration_ms: u64) -> anyhow::Result<()> {
        self.record(
            "hitl_approval_duration_ms",
            MetricType::Histogram,
            duration_ms as f64,
            &[],
        )
    }

    /// Registra uma escalation.
    pub fn record_hitl_escalation(&self, policy_name: &str) -> anyhow::Result<()> {
        self.count("hitl_escalation_total", &[("policy", policy_name)])
    }

    // ── Métricas Provider Routing (PVC-Q1.4) ──────────────────────────────────

    /// Registra uma decisão de roteamento de provider.
    pub fn record_provider_routing(
        &self,
        strategy: &str,
        provider: &str,
        complexity: &str,
    ) -> anyhow::Result<()> {
        self.count(
            "provider_routing_decision_total",
            &[
                ("strategy", strategy),
                ("provider", provider),
                ("complexity", complexity),
            ],
        )
    }

    /// Registra uso de budget como gauge (sobrescrito por sessão).
    pub fn record_budget_usage(
        &self,
        session_id: &str,
        used_usd: f64,
        max_usd: f64,
    ) -> anyhow::Result<()> {
        let point = MetricPoint {
            timestamp: now(),
            name: "budget_usage_usd".into(),
            metric_type: MetricType::Gauge,
            value: used_usd,
            labels: vec![
                ("session_id".into(), session_id.into()),
                ("max_usd".into(), format!("{:.4}", max_usd)),
            ],
        };
        self.blackboard
            .put_tuple("metrics", &format!("budget_usage_{}", session_id), serde_json::to_value(point)?)
    }

    /// Registra classificação de um request.
    pub fn record_request_classification(
        &self,
        complexity: &str,
        sensitivity: &str,
        request_type: &str,
    ) -> anyhow::Result<()> {
        self.count(
            "request_classification_total",
            &[
                ("complexity", complexity),
                ("sensitivity", sensitivity),
                ("request_type", request_type),
            ],
        )
    }

    /// Registra economia de custo em USD (counter acumulativo).
    pub fn record_cost_savings(&self, saved_usd: f64, reason: &str) -> anyhow::Result<()> {
        self.record(
            "cost_savings_total",
            MetricType::Counter,
            saved_usd.max(0.0),
            &[("reason", reason)],
        )
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_collector() -> MetricsCollector {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        MetricsCollector::new(bb)
    }

    #[test]
    fn record_and_query() {
        let col = temp_collector();
        col.count("dag.execution", &[("status", "success")])
            .unwrap();
        let pts = col.query("dag.", 10);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].name, "dag.execution");
    }

    #[test]
    fn record_contract_check_updates_adherence_rate() {
        let col = temp_collector();
        // 3 execuções, 1 violação → adherence deve ser < 1.0
        col.record_contract_check(0).unwrap();
        col.record_contract_check(0).unwrap();
        col.record_contract_check(1).unwrap();

        // query retorna o ponto mais recente (chave fixa sobrescrita)
        let rate_pts = col.query("contract_adherence_rate", 10);
        assert!(!rate_pts.is_empty());
        let rate = rate_pts[0].value; // ponto mais recente
        assert!(rate < 1.0, "adherence rate deveria ser < 1.0 quando há violações, foi {}", rate);
        assert!(rate >= 0.0, "adherence rate não pode ser negativo, foi {}", rate);
    }

    #[test]
    fn contract_adherence_perfect_when_no_violations() {
        let col = temp_collector();
        col.record_contract_check(0).unwrap();
        col.record_contract_check(0).unwrap();

        let rate_pts = col.query("contract_adherence_rate", 10);
        assert!(!rate_pts.is_empty());
        assert_eq!(rate_pts[0].value, 1.0);
    }

    #[test]
    fn hitl_decision_records_counter() {
        let col = temp_collector();
        col.record_hitl_decision("Approved", None).unwrap();
        col.record_hitl_decision("Rejected", None).unwrap();
        col.record_hitl_decision("Approved", None).unwrap();

        let pts = col.query("hitl_decision_total", 10);
        assert_eq!(pts.len(), 3);
    }

    #[test]
    fn hitl_pending_gauge_updates() {
        let col = temp_collector();
        col.record_hitl_pending(5).unwrap();
        col.record_hitl_pending(3).unwrap();

        let pts = col.query("hitl_pending_gauge", 10);
        assert!(!pts.is_empty());
        // O gauge mais recente deve ser 3
        assert_eq!(pts[0].value, 3.0);
    }

    #[test]
    fn hitl_escalation_records_counter() {
        let col = temp_collector();
        col.record_hitl_escalation("financial_tx").unwrap();
        col.record_hitl_escalation("data_deletion").unwrap();

        let pts = col.query("hitl_escalation_total", 10);
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn provider_routing_records_counter() {
        let col = temp_collector();
        col.record_provider_routing("CostOptimized", "ollama", "Simple").unwrap();
        col.record_provider_routing("QualityOptimized", "openai", "Complex").unwrap();

        let pts = col.query("provider_routing_decision_total", 10);
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn budget_usage_gauge_sobrescrito_por_sessao() {
        let col = temp_collector();
        col.record_budget_usage("sess_a", 1.5, 5.0).unwrap();
        col.record_budget_usage("sess_a", 3.0, 5.0).unwrap();

        let pts = col.query("budget_usage_usd", 10);
        // Deve haver 2 gauges porque usamos chaves diferentes? Não — mesma chave sobrescrita
        // Na verdade usamos chave format!("budget_usage_{}", session_id) então é uma só
        assert!(!pts.is_empty());
        assert_eq!(pts[0].value, 3.0);
    }

    #[test]
    fn request_classification_records_counter() {
        let col = temp_collector();
        col.record_request_classification("Simple", "Low", "QuickQuery").unwrap();
        col.record_request_classification("Complex", "High", "CodeGeneration").unwrap();

        let pts = col.query("request_classification_total", 10);
        assert_eq!(pts.len(), 2);
    }

    #[test]
    fn cost_savings_records_counter() {
        let col = temp_collector();
        col.record_cost_savings(0.05, "simple_task_cheap_provider").unwrap();
        col.record_cost_savings(0.12, "batch_processing_local").unwrap();

        let pts = col.query("cost_savings_total", 10);
        assert_eq!(pts.len(), 2);
    }
}
