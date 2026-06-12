use crate::engram_cache::EngramCache;
use crate::envelope::MemoryEnvelope;
use crate::graph::GraphStore;
use crate::lifecycle::LifecycleGovernance;
use crate::project::ProjectMemory;
use anyhow::Result;
use arreio_kernel::Blackboard;
use std::collections::HashMap;

/// Resultado de uma camada de recall.
#[derive(Debug, Clone)]
pub struct RecallResult {
    pub memory_id: String,
    pub score: f32,
    pub confidence: f32,
    pub why_retrieved: Vec<String>, // camadas que encontraram: "critical", "fts", "graph", "lexical"
    pub layer_signals: HashMap<String, f32>,
}

/// Pipeline de recuperação híbrida — determinístico, sem embeddings.
pub struct RecallPipeline {
    blackboard: Blackboard,
    graph_store: GraphStore,
    engram_cache: EngramCache,
}

impl RecallPipeline {
    pub fn new(blackboard: Blackboard) -> Self {
        let graph_store = GraphStore::new(blackboard.clone());
        let engram_cache = EngramCache::new(blackboard.clone());
        Self {
            blackboard,
            graph_store,
            engram_cache,
        }
    }

    /// Executa recall progressivo em camadas (somente Blackboard).
    /// Consulta EngramCache primeiro para queries frequentes.
    pub fn recall(&self, query: &str, limit: usize) -> Result<Vec<RecallResult>> {
        // Tenta cache primeiro
        if let Some(entry) = self.engram_cache.get(query) {
            if !entry.results.is_empty() {
                // Converte IDs de memória em RecallResult
                let mut results = Vec::new();
                for mem_id in &entry.results {
                    if let Some(tuple) = self.blackboard.get_tuple("memory", mem_id) {
                        if let Ok(mem) = serde_json::from_value::<MemoryEnvelope>(tuple) {
                            results.push(RecallResult {
                                memory_id: mem_id.clone(),
                                score: 1.0,
                                confidence: mem.confidence,
                                why_retrieved: vec!["engram_cache".into()],
                                layer_signals: {
                                    let mut m = HashMap::new();
                                    m.insert("engram_cache".into(), 1.0);
                                    m
                                },
                            });
                        }
                    }
                }
                if !results.is_empty() {
                    return Ok(results);
                }
            }
        }

        let results = self.recall_with_project(query, limit, None)?;

        // Popula cache com resultados
        let result_ids: Vec<String> = results.iter().map(|r| r.memory_id.clone()).collect();
        let _ = self.engram_cache.hit(query, result_ids);

        Ok(results)
    }

    /// Executa recall progressivo em camadas, incluindo memória durável de projeto.
    pub fn recall_with_project(
        &self,
        query: &str,
        limit: usize,
        project: Option<&ProjectMemory>,
    ) -> Result<Vec<RecallResult>> {
        let mut candidates: HashMap<String, RecallResult> = HashMap::new();
        let query_historical = LifecycleGovernance::is_historical_query(query);
        let query_terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        // Carrega todas as memórias do Blackboard (categoria "memory")
        let all = self.blackboard.search_tuples("memory", "");
        let memories: Vec<MemoryEnvelope> = all
            .iter()
            .filter_map(|(_, v)| serde_json::from_value(v.clone()).ok())
            .collect();

        // 0. Project Memory Files ( FTS sobre arquivos markdown do projeto )
        if let Some(pm) = project {
            if let Ok(idx) = pm.indexed_content() {
                let idx_lower = idx.to_lowercase();
                let matches = query_terms
                    .iter()
                    .filter(|t| idx_lower.contains(*t))
                    .count();
                if matches > 0 {
                    let score = (matches as f32 / query_terms.len().max(1) as f32) * 1.2;
                    let entry = candidates
                        .entry("project-memory".into())
                        .or_insert_with(|| RecallResult {
                            memory_id: "project-memory".into(),
                            score: 0.0,
                            confidence: 0.85,
                            why_retrieved: Vec::new(),
                            layer_signals: HashMap::new(),
                        });
                    entry.score += score;
                    entry.layer_signals.insert("project".into(), score);
                    if !entry.why_retrieved.contains(&"project".to_string()) {
                        entry.why_retrieved.push("project".into());
                    }
                }
            }
        }

        // 1. Critical memories (importance >= 0.95)
        for mem in &memories {
            if mem.importance >= 0.95 {
                let entry = candidates
                    .entry(mem.id.clone())
                    .or_insert_with(|| RecallResult {
                        memory_id: mem.id.clone(),
                        score: 0.0,
                        confidence: mem.confidence,
                        why_retrieved: Vec::new(),
                        layer_signals: HashMap::new(),
                    });
                entry.score += mem.importance * 2.0;
                entry
                    .layer_signals
                    .insert("critical".into(), mem.importance);
                if !entry.why_retrieved.contains(&"critical".to_string()) {
                    entry.why_retrieved.push("critical".into());
                }
            }
        }

        // 2. FTS (Full-Text Search simples por substring)
        for mem in &memories {
            if let Some(text) = mem.primary_text() {
                let text_lower = text.to_lowercase();
                let matches = query_terms
                    .iter()
                    .filter(|t| text_lower.contains(*t))
                    .count();
                if matches > 0 {
                    let score = (matches as f32 / query_terms.len().max(1) as f32) * 1.0;
                    let entry = candidates
                        .entry(mem.id.clone())
                        .or_insert_with(|| RecallResult {
                            memory_id: mem.id.clone(),
                            score: 0.0,
                            confidence: mem.confidence,
                            why_retrieved: Vec::new(),
                            layer_signals: HashMap::new(),
                        });
                    entry.score += score;
                    entry.layer_signals.insert("fts".into(), score);
                    if !entry.why_retrieved.contains(&"fts".to_string()) {
                        entry.why_retrieved.push("fts".into());
                    }
                }
            }
        }

        // 3. Graph walk (GraphStore real — BFS por relações entre memórias)
        for mem in &memories {
            let entity_matches = mem
                .entities
                .iter()
                .filter(|e| query_terms.iter().any(|t| e.to_lowercase().contains(t)))
                .count();
            let tag_matches = mem
                .tags
                .iter()
                .filter(|tag| query_terms.iter().any(|t| tag.to_lowercase().contains(t)))
                .count();

            // Seed score por entities/tags.
            let seed_score = ((entity_matches + tag_matches) as f32) * 0.7;
            if seed_score > 0.0 {
                let entry = candidates
                    .entry(mem.id.clone())
                    .or_insert_with(|| RecallResult {
                        memory_id: mem.id.clone(),
                        score: 0.0,
                        confidence: mem.confidence,
                        why_retrieved: Vec::new(),
                        layer_signals: HashMap::new(),
                    });
                entry.score += seed_score;
                entry.layer_signals.insert("graph_seed".into(), seed_score);
                if !entry.why_retrieved.contains(&"graph".to_string()) {
                    entry.why_retrieved.push("graph".into());
                }
            }

            // BFS pelo GraphStore a partir desta memória (até 2 hops).
            if let Ok(neighbors) = self.graph_store.walk(&mem.id, 2) {
                for (neighbor_id, walk_score) in neighbors {
                    // Evita duplicar a memória seed.
                    if neighbor_id == mem.id {
                        continue;
                    }
                    // Só inclui vizinhos que também estão no conjunto de memórias carregadas.
                    if memories.iter().any(|m| m.id == neighbor_id) {
                        let entry =
                            candidates
                                .entry(neighbor_id.clone())
                                .or_insert_with(|| RecallResult {
                                    memory_id: neighbor_id.clone(),
                                    score: 0.0,
                                    confidence: mem.confidence * walk_score,
                                    why_retrieved: Vec::new(),
                                    layer_signals: HashMap::new(),
                                });
                        entry.score += walk_score;
                        entry.layer_signals.insert("graph_walk".into(), walk_score);
                        if !entry.why_retrieved.contains(&"graph".to_string()) {
                            entry.why_retrieved.push("graph".into());
                        }
                    }
                }
            }
        }

        // 4. Lexical (sinônimos hardcoded)
        let synonyms: HashMap<&str, Vec<&str>> = [
            ("erro", vec!["falha", "bug", "exceção", "panic"]),
            ("teste", vec!["spec", "validação", "verificação"]),
            ("banco", vec!["database", "db", "sqlite", "postgres"]),
        ]
        .into_iter()
        .collect();

        for mem in &memories {
            if let Some(text) = mem.primary_text() {
                let text_lower = text.to_lowercase();
                let mut extra_matches = 0;
                for term in &query_terms {
                    if let Some(syns) = synonyms.get(term.as_str()) {
                        for syn in syns {
                            if text_lower.contains(syn) {
                                extra_matches += 1;
                            }
                        }
                    }
                }
                if extra_matches > 0 {
                    let score = (extra_matches as f32) * 0.5;
                    let entry = candidates
                        .entry(mem.id.clone())
                        .or_insert_with(|| RecallResult {
                            memory_id: mem.id.clone(),
                            score: 0.0,
                            confidence: mem.confidence,
                            why_retrieved: Vec::new(),
                            layer_signals: HashMap::new(),
                        });
                    entry.score += score;
                    entry.layer_signals.insert("lexical".into(), score);
                    if !entry.why_retrieved.contains(&"lexical".to_string()) {
                        entry.why_retrieved.push("lexical".into());
                    }
                }
            }
        }

        // Aplica lifecycle governance
        let mut results: Vec<RecallResult> = candidates
            .into_values()
            .map(|mut r| {
                if let Some(mem) = memories.iter().find(|m| m.id == r.memory_id) {
                    let (_, adjusted) = LifecycleGovernance::evaluate(mem, query_historical);
                    r.score = r.score.min(adjusted);
                }
                r
            })
            .filter(|r| r.score > 0.0)
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        results.truncate(limit);
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{MemoryEnvelope, MemoryType, ModalityRef, Scope};
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_mem(id: &str, text: &str, importance: f32) -> MemoryEnvelope {
        MemoryEnvelope {
            id: id.into(),
            scope: Scope::default(),
            memory_type: MemoryType::Episodic,
            modalities: vec![ModalityRef {
                modality_type: "text".into(),
                content: text.into(),
            }],
            importance,
            confidence: 0.9,
            entities: vec![],
            tags: vec![],
            content_hash: "abc".into(),
            created_at: 0,
        }
    }

    #[test]
    fn recall_encontra_por_fts() {
        let bb = temp_bb();
        let mem = make_mem("m1", "erro no módulo de autenticação", 0.5);
        bb.put_tuple("memory", "m1", serde_json::to_value(&mem).unwrap())
            .unwrap();

        let pipeline = RecallPipeline::new(bb);
        let res = pipeline.recall("autenticação erro", 5).unwrap();
        assert_eq!(res.len(), 1);
        assert!(res[0].why_retrieved.contains(&"fts".to_string()));
    }

    #[test]
    fn recall_critical_alto_importance() {
        let bb = temp_bb();
        let mem = make_mem("m1", "secreto", 0.98);
        bb.put_tuple("memory", "m1", serde_json::to_value(&mem).unwrap())
            .unwrap();

        let pipeline = RecallPipeline::new(bb);
        let res = pipeline.recall("qualquer coisa", 5).unwrap();
        assert_eq!(res.len(), 1);
        assert!(res[0].why_retrieved.contains(&"critical".to_string()));
    }
}
