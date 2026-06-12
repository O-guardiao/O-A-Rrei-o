use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Um diagnóstico LSP simplificado.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub severity: String, // "ERROR", "WARNING", "INFO", "HINT"
    pub message: String,
    pub code: Option<String>,
    pub source: Option<String>,
}

/// Armazena baseline de diagnósticos por arquivo.
pub struct BaselineStore {
    baselines: HashMap<String, Vec<Diagnostic>>,
}

impl BaselineStore {
    pub fn new() -> Self {
        Self {
            baselines: HashMap::new(),
        }
    }

    /// Captura baseline (diagnósticos pré-write).
    pub fn snapshot(&mut self, file: &str, diagnostics: Vec<Diagnostic>) {
        self.baselines.insert(file.to_string(), diagnostics);
    }

    /// Obtém baseline de um arquivo.
    pub fn get_baseline(&self, file: &str) -> Option<&Vec<Diagnostic>> {
        self.baselines.get(file)
    }

    /// Calcula diagnósticos delta (apenas os NOVOS).
    pub fn delta(&self, file: &str, current: &[Diagnostic]) -> Vec<Diagnostic> {
        let baseline = match self.get_baseline(file) {
            Some(b) => b,
            None => return current.to_vec(), // Sem baseline = tudo é novo
        };

        current
            .iter()
            .filter(|d| !baseline.contains(d))
            .cloned()
            .collect()
    }

    /// Remove baseline de um arquivo.
    pub fn clear(&mut self, file: &str) {
        self.baselines.remove(file);
    }
}

impl Default for BaselineStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diag(file: &str, line: usize, msg: &str) -> Diagnostic {
        Diagnostic {
            file: file.to_string(),
            line,
            column: 1,
            severity: "ERROR".to_string(),
            message: msg.to_string(),
            code: None,
            source: Some("rustc".to_string()),
        }
    }

    #[test]
    fn snapshot_and_get() {
        let mut store = BaselineStore::new();
        let diags = vec![make_diag("a.rs", 10, "old error")];
        store.snapshot("a.rs", diags);
        assert_eq!(store.get_baseline("a.rs").unwrap().len(), 1);
    }

    #[test]
    fn delta_returns_only_new() {
        let mut store = BaselineStore::new();
        store.snapshot("a.rs", vec![make_diag("a.rs", 10, "old error")]);

        let current = vec![
            make_diag("a.rs", 10, "old error"),
            make_diag("a.rs", 20, "new error"),
        ];

        let delta = store.delta("a.rs", &current);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].message, "new error");
    }

    #[test]
    fn delta_returns_all_when_no_baseline() {
        let store = BaselineStore::new();
        let current = vec![make_diag("a.rs", 10, "error")];
        let delta = store.delta("a.rs", &current);
        assert_eq!(delta.len(), 1);
    }

    #[test]
    fn clear_removes_baseline() {
        let mut store = BaselineStore::new();
        store.snapshot("a.rs", vec![make_diag("a.rs", 10, "error")]);
        store.clear("a.rs");
        assert!(store.get_baseline("a.rs").is_none());
    }
}
