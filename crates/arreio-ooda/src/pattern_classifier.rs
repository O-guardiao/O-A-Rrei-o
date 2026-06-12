//! Classificador de padrões rotineiros para IG&C (Implicit Guidance and Control).
//!
//! Substitui o bypass ingênuo (substring matching) por um classificador baseado em
//! hash SHA-256 de padrões conhecidos + threshold de confiança combinada.
//!
//! Um padrão só dispara bypass se:
//!   score_combinado = 0.5 * hash_match + 0.5 * model.confidence >= THETA_IGC
//!
//! Isso evita falsos positivos de substring e garante que apenas padrões
//! explicitamente registrados e com alta confiança do modelo disparem fast-path.

use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Threshold combinado para ativação do bypass IG&C.
pub const THETA_COMBINED: f64 = 0.90;

/// Classificador de padrões rotineiros.
#[derive(Debug, Clone)]
pub struct PatternClassifier {
    /// Hashes SHA-256 dos padrões rotineiros conhecidos (lowercase, trim).
    known_hashes: HashSet<String>,
}

impl PatternClassifier {
    /// Cria um classificador vazio.
    pub fn new() -> Self {
        Self {
            known_hashes: HashSet::new(),
        }
    }

    /// Cria um classificador pré-populado com padrões.
    pub fn with_patterns(patterns: Vec<&str>) -> Self {
        let mut classifier = Self::new();
        for p in patterns {
            classifier.register(p);
        }
        classifier
    }

    /// Registra um novo padrão rotineiro.
    pub fn register(&mut self, pattern: &str) {
        let hash = Self::hash_pattern(pattern);
        self.known_hashes.insert(hash);
    }

    /// Remove um padrão.
    pub fn unregister(&mut self, pattern: &str) {
        let hash = Self::hash_pattern(pattern);
        self.known_hashes.remove(&hash);
    }

    /// Classifica uma entrada e retorna o score combinado.
    ///
    /// Retorna `Some(score)` se o padrão for conhecido, `None` caso contrário.
    /// O caller deve combinar com `model.confidence` para decisão final.
    pub fn classify(&self, input: &str) -> Option<f64> {
        let input_hash = Self::hash_pattern(input);
        if self.known_hashes.contains(&input_hash) {
            // Hash match puro contribui com 0.5 do score.
            Some(0.5)
        } else {
            None
        }
    }

    /// Decide se o bypass IG&C deve ocorrer.
    ///
    /// Bypass só ativado quando:
    /// - O input match exato com um padrão conhecido (hash).
    /// - A confiança do modelo é alta o suficiente para compensar o score combinado.
    pub fn should_bypass(&self, input: &str, model_confidence: f64) -> bool {
        match self.classify(input) {
            Some(hash_score) => {
                let combined = hash_score + 0.5 * model_confidence;
                combined >= THETA_COMBINED
            }
            None => false,
        }
    }

    /// Retorna o número de padrões registrados.
    pub fn len(&self) -> usize {
        self.known_hashes.len()
    }

    /// Retorna true se não há padrões registrados.
    pub fn is_empty(&self) -> bool {
        self.known_hashes.is_empty()
    }

    fn hash_pattern(pattern: &str) -> String {
        let normalized = pattern.to_lowercase().trim().to_string();
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

impl Default for PatternClassifier {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_triggers_bypass_with_high_confidence() {
        let mut classifier = PatternClassifier::new();
        classifier.register("hello world");

        // confidence = 0.95 → combined = 0.5 + 0.5*0.95 = 0.975 >= 0.90
        assert!(classifier.should_bypass("hello world", 0.95));
    }

    #[test]
    fn exact_match_does_not_trigger_with_low_confidence() {
        let mut classifier = PatternClassifier::new();
        classifier.register("hello world");

        // confidence = 0.50 → combined = 0.5 + 0.5*0.50 = 0.75 < 0.90
        assert!(!classifier.should_bypass("hello world", 0.50));
    }

    #[test]
    fn substring_does_not_trigger() {
        let mut classifier = PatternClassifier::new();
        classifier.register("hello world");

        // Match por substring NÃO conta — apenas hash exato.
        assert!(!classifier.should_bypass("say hello world today", 0.95));
    }

    #[test]
    fn case_insensitive_match() {
        let mut classifier = PatternClassifier::new();
        classifier.register("Hello World");

        assert!(classifier.should_bypass("hello world", 0.95));
        assert!(classifier.should_bypass("HELLO WORLD", 0.95));
    }

    #[test]
    fn whitespace_normalized() {
        let mut classifier = PatternClassifier::new();
        classifier.register("hello   world");

        // Trim e whitespace interno é preservado no hash.
        assert!(!classifier.should_bypass("hello world", 0.95));
        assert!(classifier.should_bypass("hello   world", 0.95));
    }

    #[test]
    fn unknown_pattern_never_bypasses() {
        let classifier = PatternClassifier::new();
        assert!(!classifier.should_bypass("anything", 1.0));
    }

    #[test]
    fn unregister_removes_pattern() {
        let mut classifier = PatternClassifier::new();
        classifier.register("temp");
        assert!(classifier.should_bypass("temp", 0.95));

        classifier.unregister("temp");
        assert!(!classifier.should_bypass("temp", 0.95));
    }

    #[test]
    fn classify_returns_none_for_unknown() {
        let classifier = PatternClassifier::new();
        assert_eq!(classifier.classify("unknown"), None);
    }

    #[test]
    fn classify_returns_some_for_known() {
        let mut classifier = PatternClassifier::new();
        classifier.register("known task");
        assert_eq!(classifier.classify("known task"), Some(0.5));
    }
}
