//! Frozen Snapshot — captura estática de memória no início da sessão.
//!
//! Inspirado no padrão "Frozen Snapshot" do Hermes Agent (P5).
//! Preserva o prefix cache do LLM ao garantir que o system prompt e a memória
//! file-backed não mudem durante a sessão.
//!
//! Funcionalidade:
//! 1. Captura tuplas de uma categoria no Blackboard no momento da criação.
//! 2. Normaliza whitespace para maximizar reuso do KV cache.
//! 3. Fornece acesso imutável — nunca reflete mudanças mid-session.

use arreio_kernel::Blackboard;
use serde_json::Value;

/// Snapshot imutável de tuplas do Blackboard.
#[derive(Debug, Clone)]
pub struct FrozenSnapshot {
    /// Tuplas capturadas no momento da criação.
    entries: Vec<(String, String, Value)>, // (category, key, payload)
}

impl FrozenSnapshot {
    /// Cria um snapshot capturando todas as tuplas de uma categoria.
    pub fn capture(blackboard: &Blackboard, category: &str) -> Self {
        let tuples = blackboard.search_tuples(category, "");
        let entries: Vec<_> = tuples
            .into_iter()
            .map(|(key, payload)| (category.to_string(), key, payload))
            .collect();
        Self { entries }
    }

    /// Cria um snapshot vazio (para testes ou sessões sem memória).
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Retorna as entradas do snapshot (imutável).
    pub fn entries(&self) -> &[(String, String, Value)] {
        &self.entries
    }

    /// Monta um bloco de texto normalizado a partir das entradas.
    ///
    /// A normalização remove whitespace excessivo e garante que
    /// snapshots byte-identicamente iguais reusam o KV cache do servidor LLM.
    pub fn to_normalized_text(&self) -> String {
        let mut parts: Vec<String> = self
            .entries
            .iter()
            .map(|(_, key, payload)| {
                let text = format!("{}: {}", key, payload.to_string());
                normalize_whitespace(&text)
            })
            .collect();
        parts.sort(); // ordenação determinística para estabilidade de cache
        parts.join("\n")
    }

    /// Verifica se o snapshot contém uma chave específica.
    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(_, k, _)| k == key)
    }

    /// Retorna o payload de uma chave específica, se existir.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(_, k, _)| k == key)
            .map(|(_, _, v)| v)
    }
}

/// Normaliza whitespace para maximizar reuso de prefix cache.
///
/// Regras:
/// 1. Trim em cada linha.
/// 2. Remove linhas vazias consecutivas (max 1 linha vazia).
/// 3. Substitui múltiplos espaços internos por 1 espaço.
fn normalize_whitespace(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();
    let mut result = String::new();
    let mut last_was_empty = false;

    for line in lines {
        if line.is_empty() {
            if !last_was_empty {
                result.push('\n');
                last_was_empty = true;
            }
            continue;
        }
        last_was_empty = false;
        // Substitui múltiplos espaços por 1
        let normalized: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        result.push_str(&normalized);
        result.push('\n');
    }

    result.trim_end().to_string()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn snapshot_captures_category() {
        let bb = temp_bb();
        bb.put_tuple("memory", "user_pref", serde_json::json!("dark_mode"))
            .unwrap();
        bb.put_tuple("memory", "project_ctx", serde_json::json!("rust_project"))
            .unwrap();
        bb.put_tuple("other", "ignored", serde_json::json!("x"))
            .unwrap();

        let snap = FrozenSnapshot::capture(&bb, "memory");
        assert_eq!(snap.entries().len(), 2);
        assert!(snap.contains("user_pref"));
        assert!(snap.contains("project_ctx"));
        assert!(!snap.contains("ignored"));
    }

    #[test]
    fn snapshot_is_immutable() {
        let bb = temp_bb();
        bb.put_tuple("memory", "a", serde_json::json!(1)).unwrap();

        let snap = FrozenSnapshot::capture(&bb, "memory");
        // Modifica o Blackboard depois da captura.
        bb.put_tuple("memory", "b", serde_json::json!(2)).unwrap();

        // Snapshot não reflete a mudança.
        assert_eq!(snap.entries().len(), 1);
        assert!(!snap.contains("b"));
    }

    #[test]
    fn normalize_whitespace_collapses_spaces() {
        let input = "  hello   world  \n\n\n  foo   bar  ";
        let expected = "hello world\n\nfoo bar";
        assert_eq!(normalize_whitespace(input), expected);
    }

    #[test]
    fn to_normalized_text_is_deterministic() {
        let bb = temp_bb();
        bb.put_tuple("cfg", "b", serde_json::json!(2)).unwrap();
        bb.put_tuple("cfg", "a", serde_json::json!(1)).unwrap();

        let snap = FrozenSnapshot::capture(&bb, "cfg");
        let text1 = snap.to_normalized_text();
        let text2 = snap.to_normalized_text();
        assert_eq!(text1, text2);
        // Deve estar ordenado alfabeticamente por chave
        assert!(text1.find("a:").unwrap() < text1.find("b:").unwrap());
    }

    #[test]
    fn empty_snapshot_has_no_entries() {
        let snap = FrozenSnapshot::empty();
        assert!(snap.entries().is_empty());
        assert_eq!(snap.to_normalized_text(), "");
    }
}
