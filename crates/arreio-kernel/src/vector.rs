//! Vector Store no Blackboard — RAG como serviço (PVC-Q2.3 + PVC-Q4.2).
//!
//! Operações vetoriais como primitivas do kernel: `bb.vector_insert()`,
//! `bb.vector_query()`, `bb.vector_delete()`. Os vetores são tuplas normais
//! na categoria `vector` — herdam a persistência (JSON/SQLite) e a
//! auditabilidade do Tuple Space.
//!
//! Backends (PVC-Q4.2, ADR-0014): a estratégia de busca é plugável via o
//! trait `VectorBackend`. O default imutável é o `LinearBackend` (cosseno
//! força bruta O(n) — decisão original do ADR-0006: SQLite-vss exige
//! extensão C com build script, vetado). O `HnswBackend` (vector_hnsw.rs)
//! é opt-in via env `ARREIO_VECTOR_BACKEND` ou tupla `config::vector_backend`.
//! Toda mutação bumpa a tupla `vector_meta::rev`, usada por backends com
//! cache derivado para invalidação exata — a fonte de verdade permanece
//! sendo as tuplas (ADR-0001).

use crate::blackboard::Blackboard;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entrada do vector store (persistida como tupla `vector::<id>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorEntry {
    pub id: String,
    /// Texto original do chunk (fonte da resposta RAG).
    pub text: String,
    pub embedding: Vec<f32>,
    /// Metadados livres (origem, documento, posição, etc.).
    pub metadata: Value,
}

/// Resultado de uma consulta por similaridade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorHit {
    pub id: String,
    /// Similaridade de cosseno em [-1.0, 1.0] (maior = mais próximo).
    pub score: f32,
    pub text: String,
    pub metadata: Value,
}

/// Similaridade de cosseno. Vetores de norma zero ou dimensões diferentes
/// retornam 0.0 (nunca panic — robustez determinística).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

// ── Backend plugável (PVC-Q4.2, ADR-0014) ────────────────────────────────────

/// Estratégia de busca por similaridade. O backend é dono do carregamento
/// E da busca — um adapter de storage externo (pgvector/Qdrant) pode
/// implementar este trait sem tocar nas tuplas do Blackboard.
pub trait VectorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Busca os `top_k` mais similares. Nunca falha por config/estado —
    /// degrada para vazio (robustez determinística da API original).
    fn query(&self, bb: &Blackboard, embedding: &[f32], top_k: usize) -> Vec<VectorHit>;
}

/// Backend default: cosseno força bruta O(n) sobre as tuplas — o
/// comportamento original do PVC-Q2.3, byte-a-byte.
pub struct LinearBackend;

impl VectorBackend for LinearBackend {
    fn name(&self) -> &'static str {
        "linear"
    }

    fn query(&self, bb: &Blackboard, embedding: &[f32], top_k: usize) -> Vec<VectorHit> {
        let mut hits: Vec<VectorHit> = load_entries(bb)
            .into_iter()
            .filter(|e| e.embedding.len() == embedding.len())
            .map(|e| VectorHit {
                score: cosine_similarity(embedding, &e.embedding),
                id: e.id,
                text: e.text,
                metadata: e.metadata,
            })
            .collect();
        // Ordena por score decrescente; empate → ordem por id (determinístico).
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        hits.truncate(top_k);
        hits
    }
}

/// Carrega todas as entradas do store (tuplas `vector::*`), ordenadas por id
/// (ordem determinística para qualquer backend que dependa de ordem de
/// inserção, ex.: construção de grafo HNSW).
pub(crate) fn load_entries(bb: &Blackboard) -> Vec<VectorEntry> {
    let mut entries: Vec<VectorEntry> = bb
        .search_tuples("vector", "")
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_value::<VectorEntry>(v).ok())
        .collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    entries
}

/// Revisão atual do store (0 se nunca houve mutação registrada).
pub(crate) fn store_revision(bb: &Blackboard) -> u64 {
    bb.get_tuple("vector_meta", "rev")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// Incrementa a revisão do store — chamada em TODA mutação para que
/// backends com cache derivado invalidem com exatidão.
fn bump_revision(bb: &Blackboard) -> Result<()> {
    let next = store_revision(bb).wrapping_add(1);
    bb.put_tuple("vector_meta", "rev", serde_json::json!(next))
}

/// Resolve o backend ativo. Precedência: env `ARREIO_VECTOR_BACKEND` >
/// tupla `config::vector_backend` > `linear`. Valor desconhecido →
/// fallback linear com aviso (a consulta nunca falha por configuração).
pub fn active_vector_backend(bb: &Blackboard) -> &'static dyn VectorBackend {
    static LINEAR: LinearBackend = LinearBackend;
    let configured = std::env::var("ARREIO_VECTOR_BACKEND").ok().or_else(|| {
        bb.get_tuple("config", "vector_backend")
            .and_then(|v| v.as_str().map(String::from))
    });
    match configured.as_deref() {
        None | Some("linear") => &LINEAR,
        Some("hnsw") => crate::vector_hnsw::hnsw_backend(),
        Some(other) => {
            eprintln!(
                "[arreio-kernel] AVISO: vector backend desconhecido '{}' — usando linear",
                other
            );
            &LINEAR
        }
    }
}

impl Blackboard {
    /// Insere (ou sobrescreve) um vetor no store.
    pub fn vector_insert(
        &self,
        id: &str,
        text: &str,
        embedding: Vec<f32>,
        metadata: Value,
    ) -> Result<()> {
        let entry = VectorEntry {
            id: id.to_string(),
            text: text.to_string(),
            embedding,
            metadata,
        };
        self.put_tuple("vector", id, serde_json::to_value(&entry)?)?;
        bump_revision(self)
    }

    /// Busca os `top_k` vetores mais similares ao embedding de consulta,
    /// delegando ao backend ativo (default: linear — comportamento original).
    /// Entradas com dimensão incompatível são ignoradas (score 0 não entra).
    pub fn vector_query(&self, embedding: &[f32], top_k: usize) -> Vec<VectorHit> {
        active_vector_backend(self).query(self, embedding, top_k)
    }

    /// Busca com backend injetado explicitamente (testes e consumidores
    /// avançados — PVC-Q4.2).
    pub fn vector_query_with(
        &self,
        backend: &dyn VectorBackend,
        embedding: &[f32],
        top_k: usize,
    ) -> Vec<VectorHit> {
        backend.query(self, embedding, top_k)
    }

    /// Remove um vetor do store.
    pub fn vector_delete(&self, id: &str) -> Result<()> {
        self.delete_tuple("vector", id)?;
        bump_revision(self)
    }

    /// Número de vetores armazenados.
    pub fn vector_len(&self) -> usize {
        self.search_tuples("vector", "").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn cosine_identico_e_um() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_ortogonal_e_zero() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn cosine_dimensoes_diferentes_e_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_norma_zero_e_zero() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn insert_query_roundtrip() {
        let bb = temp_bb();
        bb.vector_insert("a", "gato", vec![1.0, 0.0], serde_json::json!({"doc": "animais"}))
            .unwrap();
        bb.vector_insert("b", "cachorro", vec![0.9, 0.1], serde_json::json!({}))
            .unwrap();
        bb.vector_insert("c", "carro", vec![0.0, 1.0], serde_json::json!({}))
            .unwrap();

        let hits = bb.vector_query(&[1.0, 0.0], 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "a");
        assert_eq!(hits[1].id, "b");
        assert!(hits[0].score > hits[1].score);
        assert_eq!(hits[0].metadata["doc"], "animais");
    }

    #[test]
    fn delete_remove_do_indice() {
        let bb = temp_bb();
        bb.vector_insert("x", "t", vec![1.0], serde_json::json!(null))
            .unwrap();
        assert_eq!(bb.vector_len(), 1);
        bb.vector_delete("x").unwrap();
        assert_eq!(bb.vector_len(), 0);
        assert!(bb.vector_query(&[1.0], 5).is_empty());
    }

    #[test]
    fn dimensao_incompativel_e_ignorada() {
        let bb = temp_bb();
        bb.vector_insert("ok", "t", vec![1.0, 0.0], serde_json::json!(null))
            .unwrap();
        bb.vector_insert("ruim", "t", vec![1.0, 0.0, 0.0], serde_json::json!(null))
            .unwrap();
        let hits = bb.vector_query(&[1.0, 0.0], 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "ok");
    }

    #[test]
    fn insert_sobrescreve_mesmo_id() {
        let bb = temp_bb();
        bb.vector_insert("a", "v1", vec![1.0], serde_json::json!(null))
            .unwrap();
        bb.vector_insert("a", "v2", vec![1.0], serde_json::json!(null))
            .unwrap();
        assert_eq!(bb.vector_len(), 1);
        let hits = bb.vector_query(&[1.0], 1);
        assert_eq!(hits[0].text, "v2");
    }

    #[test]
    fn backend_default_e_linear_sem_config() {
        let bb = temp_bb();
        assert_eq!(active_vector_backend(&bb).name(), "linear");
    }

    #[test]
    fn selecao_por_tupla_config_e_fallback_para_desconhecido() {
        let bb = temp_bb();
        bb.put_tuple("config", "vector_backend", serde_json::json!("hnsw"))
            .unwrap();
        assert_eq!(active_vector_backend(&bb).name(), "hnsw");

        // Valor desconhecido: fallback linear, consulta nunca falha.
        bb.put_tuple("config", "vector_backend", serde_json::json!("marciano"))
            .unwrap();
        assert_eq!(active_vector_backend(&bb).name(), "linear");
        bb.vector_insert("a", "t", vec![1.0], serde_json::json!(null))
            .unwrap();
        assert_eq!(bb.vector_query(&[1.0], 1).len(), 1);
    }

    #[test]
    fn revisao_incrementa_em_insert_e_delete() {
        let bb = temp_bb();
        assert_eq!(store_revision(&bb), 0);
        bb.vector_insert("a", "t", vec![1.0], serde_json::json!(null))
            .unwrap();
        assert_eq!(store_revision(&bb), 1);
        bb.vector_delete("a").unwrap();
        assert_eq!(store_revision(&bb), 2);
    }

    #[test]
    fn vector_query_with_injeta_backend_explicito() {
        let bb = temp_bb();
        bb.vector_insert("a", "t", vec![1.0, 0.0], serde_json::json!(null))
            .unwrap();
        let hits = bb.vector_query_with(&LinearBackend, &[1.0, 0.0], 1);
        assert_eq!(hits[0].id, "a");
    }

    #[test]
    fn tupla_de_revisao_nao_vaza_no_store_nem_no_len() {
        let bb = temp_bb();
        bb.vector_insert("a", "t", vec![1.0], serde_json::json!(null))
            .unwrap();
        // `vector_meta::rev` não pode aparecer como entrada nem inflar o len.
        assert_eq!(bb.vector_len(), 1);
        assert_eq!(load_entries(&bb).len(), 1);
    }

    #[test]
    fn vetores_persistem_no_disco() {
        let f = NamedTempFile::new().unwrap();
        let path: PathBuf = f.path().to_path_buf();
        drop(f);
        {
            let bb = Blackboard::open(&path).unwrap();
            bb.vector_insert("p", "persistido", vec![0.5, 0.5], serde_json::json!(null))
                .unwrap();
        }
        let reopened = Blackboard::open(&path).unwrap();
        assert_eq!(reopened.vector_len(), 1);
        let hits = reopened.vector_query(&[0.5, 0.5], 1);
        assert_eq!(hits[0].text, "persistido");
    }
}
