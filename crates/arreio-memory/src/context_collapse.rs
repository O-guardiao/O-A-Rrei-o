//! Context Collapse — GAP-013
//!
//! Projeção virtual sobre história arquivada no Blackboard.
//! Não muta a história armazenada — apenas substitui a visão colapsada
//! quando o contexto excede um threshold.
//!
//! Inspirado no Context Collapse do Claude Code: projeção em tempo de leitura
//! que reduz tokens sem perder informação crítica.

use arreio_kernel::Blackboard;
use serde_json::Value;

/// Trait para sumarização de entradas colapsadas.
/// Implementações podem usar LLM, heurística avançada, ou qualquer outra técnica.
pub trait Summarizer: Send + Sync {
    /// Recebe uma lista de entradas antigas e retorna um sumário textual.
    /// Retorna `None` se não conseguir sumarizar.
    fn summarize(&self, category: &str, entries: &[(String, Value)]) -> Option<String>;
}

/// Colapsador de contexto com projeção virtual.
pub struct ContextCollapser {
    /// Número de entradas antes de colapsar.
    pub threshold: usize,
    /// Número de entradas recentes a manter intactas.
    pub keep_recent: usize,
    /// Sumarizador opcional para gerar descrições textuais das entradas colapsadas.
    /// Se `None`, usa heurística estatística (padrão).
    pub summarizer: Option<Box<dyn Summarizer>>,
}

impl ContextCollapser {
    /// Cria com threshold padrão (50 entradas) e 10 recentes.
    pub fn new() -> Self {
        Self {
            threshold: 50,
            keep_recent: 10,
            summarizer: None,
        }
    }

    /// Cria com threshold customizado.
    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            threshold,
            keep_recent: threshold / 5,
            summarizer: None,
        }
    }

    /// Cria a partir da variável de ambiente `ARREIO_COLLAPSE_THRESHOLD`.
    pub fn from_env() -> Self {
        let threshold = std::env::var("ARREIO_COLLAPSE_THRESHOLD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        Self::with_threshold(threshold)
    }

    /// Define um sumarizador customizado.
    pub fn with_summarizer(mut self, summarizer: Box<dyn Summarizer>) -> Self {
        self.summarizer = Some(summarizer);
        self
    }

    /// Colapsa entradas de uma categoria do Blackboard.
    ///
    /// - Se total < threshold: retorna todas as entradas.
    /// - Se total >= threshold: mantém `keep_recent` intactas, colapsa o resto.
    ///
    /// Regras de segurança: entradas da categoria "security" ou "permission"
    /// NUNCA são colapsadas.
    pub fn collapse(&self, blackboard: &Blackboard, category: &str) -> Vec<Value> {
        let mut all_entries = blackboard.search_tuples(category, "");
        // Ordena por chave para determinismo (ex: node_0, node_1, ...)
        all_entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Segurança e permissão nunca colapsam
        if category == "security" || category == "permission" || category == "audit" {
            return all_entries.into_iter().map(|(_, v)| v).collect();
        }

        if all_entries.len() < self.threshold {
            return all_entries.into_iter().map(|(_, v)| v).collect();
        }

        // Mantém entradas recentes intactas
        let recent_start = all_entries.len().saturating_sub(self.keep_recent);
        let old_entries: Vec<(String, Value)> = all_entries[..recent_start].to_vec();
        let recent_entries: Vec<Value> = all_entries[recent_start..]
            .iter()
            .map(|(_, v)| v.clone())
            .collect();

        // Colapsa entradas antigas
        let collapsed = self.summarize_old_entries(category, &old_entries);

        let mut result = vec![collapsed];
        result.extend(recent_entries);
        result
    }

    /// Sumariza entradas antigas em um único valor colapsado.
    ///
    /// Se um `Summarizer` estiver configurado, tenta gerar sumário textual.
    /// Em caso de falha ou ausência de sumarizador, cai na heurística estatística.
    fn summarize_old_entries(&self, category: &str, entries: &[(String, Value)]) -> Value {
        let total = entries.len();

        // Tenta sumarização textual via Summarizer
        if let Some(summarizer) = &self.summarizer {
            if let Some(summary_text) = summarizer.summarize(category, entries) {
                return serde_json::json!({
                    "__type": "collapsed_summary",
                    "__category": category,
                    "collapsed_count": total,
                    "summary": summary_text,
                    "timestamp": now(),
                });
            }
        }

        // Fallback: heurística estatística
        let successes = entries
            .iter()
            .filter(|(_, v)| v.get("result") == Some(&Value::String("Success".to_string())))
            .count();
        let failures = entries
            .iter()
            .filter(|(_, v)| v.get("result") == Some(&Value::String("Failure".to_string())))
            .count();
        let timeouts = entries
            .iter()
            .filter(|(_, v)| v.get("result") == Some(&Value::String("Timeout".to_string())))
            .count();
        let blocked = entries
            .iter()
            .filter(|(_, v)| v.get("result") == Some(&Value::String("Blocked".to_string())))
            .count();

        // Coleta modelos usados
        let mut models_used = std::collections::HashSet::new();
        let mut total_tokens: u64 = 0;
        let mut total_duration_ms: u64 = 0;

        for (_, v) in entries {
            if let Some(models) = v.get("models_used").and_then(|m| m.as_array()) {
                for m in models {
                    if let Some(s) = m.as_str() {
                        models_used.insert(s.to_string());
                    }
                }
            }
            if let Some(tokens) = v.get("tokens_consumed").and_then(|t| t.as_u64()) {
                total_tokens += tokens;
            }
            if let Some(dur) = v.get("duration_ms").and_then(|d| d.as_u64()) {
                total_duration_ms += dur;
            }
        }

        serde_json::json!({
            "__type": "collapsed_summary",
            "__category": category,
            "collapsed_count": total,
            "successes": successes,
            "failures": failures,
            "timeouts": timeouts,
            "blocked": blocked,
            "models_used": models_used.into_iter().collect::<Vec<String>>(),
            "total_tokens_consumed": total_tokens,
            "total_duration_ms": total_duration_ms,
            "timestamp": now(),
        })
    }
}

impl Default for ContextCollapser {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use tempfile::NamedTempFile;

    fn temp_blackboard() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    #[test]
    fn no_collapse_below_threshold() {
        let bb = temp_blackboard();
        let collapser = ContextCollapser::with_threshold(10);
        for i in 0..5 {
            bb.put_tuple("dag", &format!("node_{}", i), serde_json::json!({"result": "Success"})).unwrap();
        }
        let result = collapser.collapse(&bb, "dag");
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn collapses_above_threshold() {
        let bb = temp_blackboard();
        let collapser = ContextCollapser::with_threshold(10);
        for i in 0..15 {
            bb.put_tuple("dag", &format!("node_{}", i), serde_json::json!({"result": "Success"})).unwrap();
        }
        let result = collapser.collapse(&bb, "dag");
        // 15 entradas, threshold 10, keep_recent = 2
        // 1 colapsado (13 antigas) + 2 recentes = 3
        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["__type"], "collapsed_summary");
        assert_eq!(result[0]["collapsed_count"], 13);
    }

    #[test]
    fn security_never_collapses() {
        let bb = temp_blackboard();
        let collapser = ContextCollapser::with_threshold(5);
        for i in 0..10 {
            bb.put_tuple("security", &format!("rule_{}", i), serde_json::json!({"allow": true})).unwrap();
        }
        let result = collapser.collapse(&bb, "security");
        assert_eq!(result.len(), 10);
    }

    #[test]
    fn summary_counts_results() {
        let bb = temp_blackboard();
        let collapser = ContextCollapser::with_threshold(10);
        for i in 0..10 {
            let result = match i % 4 {
                0 => "Success",
                1 => "Failure",
                2 => "Timeout",
                _ => "Blocked",
            };
            bb.put_tuple("dag", &format!("node_{}", i), serde_json::json!({"result": result})).unwrap();
        }
        let result = collapser.collapse(&bb, "dag");
        let summary = &result[0];
        // 8 entradas antigas colapsadas:
        // i=0 Success, i=1 Failure, i=2 Timeout, i=3 Blocked
        // i=4 Success, i=5 Failure, i=6 Timeout, i=7 Blocked
        assert_eq!(summary["successes"], 2);
        assert_eq!(summary["failures"], 2);
        assert_eq!(summary["timeouts"], 2);
        assert_eq!(summary["blocked"], 2);
    }

    /// MockSummarizer para testes — sempre retorna um sumário fixo.
    struct MockSummarizer;

    impl Summarizer for MockSummarizer {
        fn summarize(&self, _category: &str, entries: &[(String, Value)]) -> Option<String> {
            Some(format!("Sumarizado {} entradas antigas", entries.len()))
        }
    }

    #[test]
    fn collapse_with_summarizer_uses_textual_summary() {
        let bb = temp_blackboard();
        let collapser = ContextCollapser::with_threshold(10)
            .with_summarizer(Box::new(MockSummarizer));
        for i in 0..15 {
            bb.put_tuple("dag", &format!("node_{}", i), serde_json::json!({"result": "Success"})).unwrap();
        }
        let result = collapser.collapse(&bb, "dag");
        assert_eq!(result.len(), 3);
        let summary = &result[0];
        assert_eq!(summary["__type"], "collapsed_summary");
        assert_eq!(summary["summary"], "Sumarizado 13 entradas antigas");
        // Não deve conter campos estatísticos quando summarizer funciona
        assert!(summary.get("successes").is_none());
    }

    #[test]
    fn collapse_without_summarizer_uses_heuristic_summary() {
        let bb = temp_blackboard();
        let collapser = ContextCollapser::with_threshold(10);
        for i in 0..15 {
            bb.put_tuple("dag", &format!("node_{}", i), serde_json::json!({"result": "Success"})).unwrap();
        }
        let result = collapser.collapse(&bb, "dag");
        assert_eq!(result.len(), 3);
        let summary = &result[0];
        assert_eq!(summary["__type"], "collapsed_summary");
        assert!(summary.get("successes").is_some());
        assert!(summary.get("summary").is_none());
    }
}
