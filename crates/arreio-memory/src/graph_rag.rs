//! GraphRAG — vector search integrado ao GraphStore (PVC-Q2.3).
//!
//! Combina os dois mecanismos de recall já existentes no Arreio:
//! 1. **Vector search** (`bb.vector_query`) encontra os chunks semanticamente
//!    próximos da consulta;
//! 2. **GraphStore.walk** expande cada hit pelas relações simbólicas
//!    (implements, resolves, depends_on, ...) com decay de score por hop.
//!
//! O resultado é um ranking único e determinístico. O pipeline é uma tool a
//! serviço do Planner — NUNCA augmentation automática de prompt (regra Q2.3).

use crate::graph::GraphStore;
use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Origem de um item do resultado GraphRAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RagSource {
    /// Encontrado diretamente por similaridade vetorial.
    Vector,
    /// Alcançado por expansão no grafo a partir de um hit vetorial.
    Graph,
}

/// Item ranqueado do resultado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRagResult {
    pub id: String,
    pub score: f32,
    pub source: RagSource,
    /// Texto do chunk quando disponível no vector store (hits de grafo podem
    /// referenciar memórias sem texto indexado).
    pub text: Option<String>,
}

/// Pipeline GraphRAG sobre o Blackboard.
pub struct GraphRagPipeline {
    blackboard: Blackboard,
}

impl GraphRagPipeline {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Consulta combinada: vector top_k + expansão de grafo com `max_hops`.
    /// O score de um vizinho de grafo é `score_do_hit × score_do_walk`
    /// (o walk já aplica decay de 0.9 por hop × confidence da relação).
    pub fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
        max_hops: usize,
    ) -> Result<Vec<GraphRagResult>> {
        let vector_hits = self.blackboard.vector_query(embedding, top_k);
        let graph = GraphStore::new(self.blackboard.clone());

        let mut merged: std::collections::HashMap<String, GraphRagResult> =
            std::collections::HashMap::new();

        for hit in &vector_hits {
            merged.insert(
                hit.id.clone(),
                GraphRagResult {
                    id: hit.id.clone(),
                    score: hit.score,
                    source: RagSource::Vector,
                    text: Some(hit.text.clone()),
                },
            );
        }

        if max_hops > 0 {
            for hit in &vector_hits {
                for (neighbor_id, walk_score) in graph.walk(&hit.id, max_hops)? {
                    let combined = hit.score * walk_score;
                    // Texto do vizinho, se ele também estiver no vector store.
                    let text = self
                        .blackboard
                        .get_tuple("vector", &neighbor_id)
                        .and_then(|v| {
                            serde_json::from_value::<arreio_kernel::VectorEntry>(v).ok()
                        })
                        .map(|e| e.text);
                    merged
                        .entry(neighbor_id.clone())
                        .and_modify(|existing| {
                            // Mantém sempre o maior score para o mesmo id.
                            if combined > existing.score {
                                existing.score = combined;
                            }
                        })
                        .or_insert(GraphRagResult {
                            id: neighbor_id,
                            score: combined,
                            source: RagSource::Graph,
                            text,
                        });
                }
            }
        }

        let mut results: Vec<GraphRagResult> = merged.into_values().collect();
        // Ordenação determinística: score desc, empate por id.
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Relation;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn query_apenas_vetorial_sem_hops() {
        let bb = temp_bb();
        bb.vector_insert("a", "texto a", vec![1.0, 0.0], serde_json::json!(null))
            .unwrap();
        bb.vector_insert("b", "texto b", vec![0.0, 1.0], serde_json::json!(null))
            .unwrap();

        let pipeline = GraphRagPipeline::new(bb);
        let results = pipeline.query(&[1.0, 0.0], 1, 0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
        assert_eq!(results[0].source, RagSource::Vector);
        assert_eq!(results[0].text.as_deref(), Some("texto a"));
    }

    #[test]
    fn expansao_de_grafo_traz_vizinhos() {
        let bb = temp_bb();
        bb.vector_insert("doc1", "chunk indexado", vec![1.0, 0.0], serde_json::json!(null))
            .unwrap();

        let graph = GraphStore::new(bb.clone());
        graph
            .add_relation(&Relation {
                subject: "doc1".into(),
                predicate: "depends_on".into(),
                object: "doc2".into(),
                confidence: 1.0,
            })
            .unwrap();

        let pipeline = GraphRagPipeline::new(bb);
        let results = pipeline.query(&[1.0, 0.0], 1, 1).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "doc1"); // hit direto rankeia acima
        assert_eq!(results[1].id, "doc2");
        assert_eq!(results[1].source, RagSource::Graph);
        // score do vizinho = 1.0 (cos) × 1.0 (confidence) × 0.9 (decay)
        assert!((results[1].score - 0.9).abs() < 1e-5);
    }

    #[test]
    fn vizinho_indexado_traz_texto() {
        let bb = temp_bb();
        bb.vector_insert("m1", "memoria um", vec![1.0], serde_json::json!(null))
            .unwrap();
        bb.vector_insert("m2", "memoria dois", vec![-1.0], serde_json::json!(null))
            .unwrap();

        let graph = GraphStore::new(bb.clone());
        graph
            .add_relation(&Relation {
                subject: "m1".into(),
                predicate: "resolves".into(),
                object: "m2".into(),
                confidence: 0.8,
            })
            .unwrap();

        let pipeline = GraphRagPipeline::new(bb);
        let results = pipeline.query(&[1.0], 1, 1).unwrap();
        let m2 = results.iter().find(|r| r.id == "m2").unwrap();
        assert_eq!(m2.text.as_deref(), Some("memoria dois"));
        assert_eq!(m2.source, RagSource::Graph);
    }

    #[test]
    fn id_duplicado_mantem_maior_score() {
        let bb = temp_bb();
        // "b" é hit vetorial direto E vizinho de "a" no grafo.
        bb.vector_insert("a", "ta", vec![1.0, 0.0], serde_json::json!(null))
            .unwrap();
        bb.vector_insert("b", "tb", vec![0.95, 0.05], serde_json::json!(null))
            .unwrap();
        let graph = GraphStore::new(bb.clone());
        graph
            .add_relation(&Relation {
                subject: "a".into(),
                predicate: "link".into(),
                object: "b".into(),
                confidence: 0.1,
            })
            .unwrap();

        let pipeline = GraphRagPipeline::new(bb);
        let results = pipeline.query(&[1.0, 0.0], 2, 1).unwrap();
        let b = results.iter().find(|r| r.id == "b").unwrap();
        // Score vetorial direto (~0.998) > expansão (1.0×0.1×0.9 = 0.09).
        assert!(b.score > 0.9);
        assert_eq!(b.source, RagSource::Vector);
    }
}
