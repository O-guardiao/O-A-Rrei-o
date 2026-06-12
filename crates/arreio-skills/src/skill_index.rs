//! Skill Index — progressive disclosure de skills.
//!
//! Inspirado no padrão "Progressive Disclosure" do Hermes Agent.
//! Mantém apenas metadata (frontmatter) carregada no system prompt (~10KB).
//! O conteúdo completo (body, examples, quick_reference) só é carregado
//! quando o trigger match excede o threshold de relevância.
//!
//! Isso evita inflar o contexto com skills irrelevantes na maioria dos turnos.

use crate::store::SkillStore;
use std::collections::HashMap;

/// Threshold de trigger match para carregar conteúdo completo.
pub const DISCLOSURE_THRESHOLD: f32 = 0.6;

/// Índice leve de skills — apenas metadata + triggers.
#[derive(Debug, Clone)]
pub struct SkillIndexEntry {
    pub name: String,
    pub description: String,
    pub version: String,
    pub tags: Vec<String>,
    pub trigger_keywords: Vec<String>,
    /// Score de relevância calculado no último match (0.0–1.0).
    pub last_relevance: f32,
}

impl SkillIndexEntry {
    /// Formata apenas a metadata para injeção no contexto (~100–200 bytes).
    pub fn to_metadata_line(&self) -> String {
        format!(
            "- {} v{}: {} [tags: {}]",
            self.name,
            self.version,
            self.description,
            self.tags.join(", ")
        )
    }
}

/// Índice de skills com progressive disclosure.
pub struct SkillIndex {
    store: SkillStore,
    /// Cache de metadata em memória (sempre carregado).
    metadata: HashMap<String, SkillIndexEntry>,
}

impl SkillIndex {
    pub fn new(store: SkillStore) -> Self {
        let mut idx = Self {
            store,
            metadata: HashMap::new(),
        };
        idx.rebuild();
        idx
    }

    /// Reconstrói o índice a partir do SkillStore.
    pub fn rebuild(&mut self) {
        self.metadata.clear();
        // Carrega todas as skills do store e extrai metadata.
        // Nota: SkillStore armazena Skill (JSON), não SkillMd.
        // Para o índice, usamos os campos leves de Skill.
        for skill in self.store.list() {
            let entry = SkillIndexEntry {
                name: skill.name.clone(),
                description: skill.description.clone(),
                version: "1.0.0".into(), // Skill não tem version; usamos default
                tags: skill.trigger_patterns.clone(),
                trigger_keywords: skill.trigger_patterns.clone(),
                last_relevance: 0.0,
            };
            self.metadata.insert(skill.name, entry);
        }
    }

    /// Retorna metadata de TODAS as skills (~10KB para 50 skills).
    pub fn all_metadata(&self) -> Vec<&SkillIndexEntry> {
        self.metadata.values().collect()
    }

    /// Calcula relevância de uma query contra as keywords de uma skill.
    fn relevance(query: &str, keywords: &[String]) -> f32 {
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();
        if words.is_empty() || keywords.is_empty() {
            return 0.0;
        }
        let matches = keywords
            .iter()
            .filter(|kw| {
                words
                    .iter()
                    .any(|w| w.contains(&kw.to_lowercase()) || kw.to_lowercase().contains(w))
            })
            .count();
        (matches as f32 / keywords.len().max(words.len()) as f32).min(1.0)
    }

    /// Encontra skills relevantes e decide quais carregar completamente.
    ///
    /// Retorna `(metadata_lines, full_skills)`:
    /// - `metadata_lines`: string com metadata de TODAS as skills (sempre presente).
    /// - `full_skills`: skills cujo conteúdo completo deve ser carregado.
    pub fn query(&mut self, query: &str) -> (String, Vec<String>) {
        let mut full_skills = Vec::new();

        for entry in self.metadata.values_mut() {
            let rel = Self::relevance(query, &entry.trigger_keywords);
            entry.last_relevance = rel;
            if rel >= DISCLOSURE_THRESHOLD {
                full_skills.push(entry.name.clone());
            }
        }

        let metadata_lines = self
            .metadata
            .values()
            .map(|e| e.to_metadata_line())
            .collect::<Vec<_>>()
            .join("\n");

        (metadata_lines, full_skills)
    }

    /// Retorna o número de skills indexadas.
    pub fn len(&self) -> usize {
        self.metadata.len()
    }

    pub fn is_empty(&self) -> bool {
        self.metadata.is_empty()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_skill(name: &str, patterns: Vec<&str>, desc: &str) -> crate::store::Skill {
        crate::store::Skill {
            name: name.into(),
            description: desc.into(),
            trigger_patterns: patterns.into_iter().map(String::from).collect(),
            ast_signature: None,
            file_target_pattern: None,
            instruction_template: "do it".into(),
            steps: vec!["step1".into(), "step2".into()],
            templates: Default::default(),
            validation_cmds: vec![],
            last_used: 0,
            usage_count: 1,
            success_rate: 1.0,
            created_from_dag_task_id: None,
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: crate::store::SkillTrust::Trusted,
            module_count: 1,
            mutation_history: vec![],
        }
    }

    #[test]
    fn index_loads_metadata_only() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        store
            .save(&make_skill("auth", vec!["login", "jwt"], "autenticação"))
            .unwrap();
        store
            .save(&make_skill(
                "db",
                vec!["sqlite", "migration"],
                "banco de dados",
            ))
            .unwrap();

        let idx = SkillIndex::new(store);
        assert_eq!(idx.len(), 2);
        // Metadata não contém steps (que existem na skill completa).
        let meta = idx.all_metadata();
        assert!(!meta[0].to_metadata_line().contains("step1"));
    }

    #[test]
    fn query_returns_metadata_for_all_and_full_for_matching() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        store
            .save(&make_skill("auth", vec!["login", "jwt"], "autenticação"))
            .unwrap();
        store
            .save(&make_skill(
                "db",
                vec!["sqlite", "migration"],
                "banco de dados",
            ))
            .unwrap();

        let mut idx = SkillIndex::new(store);
        let (metadata, full) = idx.query("login jwt");

        // Metadata contém ambas as skills.
        assert!(metadata.contains("auth"));
        assert!(metadata.contains("db"));

        // Apenas "auth" tem match alto o suficiente (2 keywords match / 2 keywords = 1.0).
        assert_eq!(full.len(), 1);
        assert_eq!(full[0], "auth");
    }

    #[test]
    fn low_relevance_does_not_trigger_full_load() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        store
            .save(&make_skill("auth", vec!["login", "jwt"], "autenticação"))
            .unwrap();

        let mut idx = SkillIndex::new(store);
        let (_metadata, full) = idx.query("completely unrelated topic");

        assert!(full.is_empty());
    }

    #[test]
    fn metadata_size_is_small() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        for i in 0..50 {
            store
                .save(&make_skill(
                    &format!("skill_{}", i),
                    vec![&format!("kw{}", i)],
                    &format!("descrição da skill número {}", i),
                ))
                .unwrap();
        }

        let idx = SkillIndex::new(store);
        let meta = idx.all_metadata();
        let total_chars: usize = meta.iter().map(|e| e.to_metadata_line().len()).sum();
        // 50 skills × ~100 chars = ~5000 bytes (bem abaixo de 10KB).
        assert!(
            total_chars < 10_000,
            "metadata deve ser < 10KB, obtido {} bytes",
            total_chars
        );
    }
}
