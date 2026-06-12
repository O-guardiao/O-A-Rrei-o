//! HnswBackend — busca vetorial aproximada HNSW, determinística (PVC-Q4.2).
//!
//! Implementação em Rust puro (zero dependências novas) do Hierarchical
//! Navigable Small World, adaptada às regras do Arreio:
//!
//! - **Sem RNG**: o nível de cada nó deriva de hash FNV-1a do id — o mesmo
//!   conjunto de vetores SEMPRE produz o mesmo grafo e os mesmos resultados.
//! - **Cache derivado, nunca autoritativo** (ADR-0001): o índice vive em
//!   memória chaveado por `(store_path, rev)`; a tupla `vector_meta::rev`
//!   (bumpada em toda mutação) garante invalidação exata. A fonte de verdade
//!   permanece sendo as tuplas `vector::*` do Blackboard.
//! - **Aproximado e opt-in** (ADR-0014): resultados podem diferir do backend
//!   linear; recall mínimo é verificado por teste. O default do kernel
//!   continua sendo o `LinearBackend`.

use crate::blackboard::Blackboard;
use crate::vector::{
    cosine_similarity, load_entries, store_revision, VectorBackend, VectorEntry, VectorHit,
};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// Vizinhos máximos por nó nos níveis > 0.
const M: usize = 16;
/// Vizinhos máximos no nível 0 (base, mais denso).
const M0: usize = 32;
/// Largura do beam na construção.
const EF_CONSTRUCTION: usize = 100;
/// Largura mínima do beam na busca (cresce com top_k).
const EF_SEARCH_MIN: usize = 64;
/// Teto de níveis (segurança contra hashes extremos).
const MAX_LEVEL_CAP: usize = 16;

// ── Determinismo ──────────────────────────────────────────────────────────────

/// FNV-1a 64 bits — hash determinístico do id do vetor.
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Nível do nó: distribuição geométrica do HNSW clássico, mas com a
/// "aleatoriedade" derivada do hash do id (reprodutível para sempre).
fn deterministic_level(id: &str) -> usize {
    // u em (0, 1]: 53 bits do hash mapeados para o intervalo unitário.
    let u = ((fnv1a64(id) >> 11) as f64 + 1.0) / ((1u64 << 53) as f64);
    let level = (-u.ln() * std::f64::consts::LOG2_E.recip().recip()).floor();
    // mL = 1/ln(2) — fator clássico; reescrito para evitar constante mágica:
    let m_l = 1.0 / std::f64::consts::LN_2;
    let level = (-u.ln() * m_l).floor().max(level.min(0.0));
    (level as usize).min(MAX_LEVEL_CAP)
}

/// Distância usada no grafo: 1 − cosseno (menor = mais próximo).
/// Dimensões incompatíveis → cosseno 0 → distância 1 (longe de tudo).
fn dist(a: &[f32], b: &[f32]) -> f32 {
    1.0 - cosine_similarity(a, b)
}

/// Par (distância, índice) com ordenação TOTAL e determinística
/// (`total_cmp` + desempate por índice) para uso em heaps.
#[derive(PartialEq)]
struct Hd(f32, usize);

impl Eq for Hd {}

impl Ord for Hd {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .total_cmp(&other.0)
            .then_with(|| self.1.cmp(&other.1))
    }
}

impl PartialOrd for Hd {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ── Grafo ─────────────────────────────────────────────────────────────────────

/// Grafo HNSW construído sobre entradas ordenadas por id.
struct HnswGraph {
    /// `neighbors[no][nivel]` → vizinhos (índices em `entries`).
    neighbors: Vec<Vec<Vec<usize>>>,
    entry_point: usize,
    max_level: usize,
}

impl HnswGraph {
    fn build(entries: &[VectorEntry]) -> Self {
        let mut g = HnswGraph {
            neighbors: Vec::with_capacity(entries.len()),
            entry_point: 0,
            max_level: 0,
        };
        for i in 0..entries.len() {
            let level = deterministic_level(&entries[i].id);
            g.neighbors.push(vec![Vec::new(); level + 1]);
            if i == 0 {
                g.max_level = level;
                continue;
            }
            g.insert_node(entries, i, level);
            if level > g.max_level {
                g.max_level = level;
                g.entry_point = i;
            }
        }
        g
    }

    fn insert_node(&mut self, entries: &[VectorEntry], i: usize, level: usize) {
        let query = &entries[i].embedding;
        let mut cur = self.entry_point;

        // Descida greedy nos níveis acima do nível do novo nó.
        if self.max_level > level {
            for l in ((level + 1)..=self.max_level).rev() {
                cur = self.greedy_closest(entries, query, cur, l);
            }
        }

        // Conexões do nível min(level, max_level) até a base.
        for l in (0..=level.min(self.max_level)).rev() {
            let candidates = self.search_layer(entries, query, cur, l, EF_CONSTRUCTION);
            let m_max = if l == 0 { M0 } else { M };
            for &(nb, _) in candidates.iter().take(M) {
                if nb == i {
                    continue;
                }
                if !self.neighbors[i][l].contains(&nb) {
                    self.neighbors[i][l].push(nb);
                }
                if !self.neighbors[nb][l].contains(&i) {
                    self.neighbors[nb][l].push(i);
                    if self.neighbors[nb][l].len() > m_max {
                        self.prune(entries, nb, l, m_max);
                    }
                }
            }
            if let Some(&(best, _)) = candidates.first() {
                cur = best;
            }
        }
    }

    /// Mantém apenas os `m_max` vizinhos mais próximos (determinístico).
    fn prune(&mut self, entries: &[VectorEntry], node: usize, level: usize, m_max: usize) {
        let q = &entries[node].embedding;
        let uniq: HashSet<usize> = self.neighbors[node][level].iter().copied().collect();
        let mut nbs: Vec<usize> = uniq.into_iter().collect();
        nbs.sort_by(|&a, &b| {
            dist(q, &entries[a].embedding)
                .total_cmp(&dist(q, &entries[b].embedding))
                .then_with(|| a.cmp(&b))
        });
        nbs.truncate(m_max);
        self.neighbors[node][level] = nbs;
    }

    /// Caminhada greedy em um nível: segue para o vizinho mais próximo até
    /// não haver melhora (desempate por índice — determinístico).
    fn greedy_closest(
        &self,
        entries: &[VectorEntry],
        query: &[f32],
        mut cur: usize,
        level: usize,
    ) -> usize {
        let mut cur_d = dist(query, &entries[cur].embedding);
        loop {
            let mut improved = false;
            // Clona a lista (≤ M0) para evitar conflito de borrow ao mover `cur`.
            let nbs: Vec<usize> = match self.neighbors[cur].get(level) {
                Some(v) => v.clone(),
                None => Vec::new(),
            };
            for nb in nbs {
                let d = dist(query, &entries[nb].embedding);
                if d < cur_d {
                    cur = nb;
                    cur_d = d;
                    improved = true;
                }
            }
            if !improved {
                return cur;
            }
        }
    }

    /// Beam search (best-first) em um nível. Retorna até `ef` pares
    /// (índice, distância) ordenados por distância crescente.
    fn search_layer(
        &self,
        entries: &[VectorEntry],
        query: &[f32],
        entry: usize,
        level: usize,
        ef: usize,
    ) -> Vec<(usize, f32)> {
        let mut visited: HashSet<usize> = HashSet::new();
        visited.insert(entry);
        let d0 = dist(query, &entries[entry].embedding);

        // Candidatos: min-heap por distância. Resultados: max-heap (pior no topo).
        let mut candidates: BinaryHeap<Reverse<Hd>> = BinaryHeap::new();
        candidates.push(Reverse(Hd(d0, entry)));
        let mut results: BinaryHeap<Hd> = BinaryHeap::new();
        results.push(Hd(d0, entry));

        while let Some(Reverse(Hd(dc, c))) = candidates.pop() {
            let worst = results.peek().map(|h| h.0).unwrap_or(f32::INFINITY);
            if dc > worst && results.len() >= ef {
                break;
            }
            let nbs: &[usize] = match self.neighbors[c].get(level) {
                Some(v) => v,
                None => &[],
            };
            for &nb in nbs {
                if !visited.insert(nb) {
                    continue;
                }
                let dn = dist(query, &entries[nb].embedding);
                let worst = results.peek().map(|h| h.0).unwrap_or(f32::INFINITY);
                if results.len() < ef || dn < worst {
                    candidates.push(Reverse(Hd(dn, nb)));
                    results.push(Hd(dn, nb));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        let mut out: Vec<(usize, f32)> = results.into_iter().map(|Hd(d, i)| (i, d)).collect();
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Busca completa: descida greedy até o nível 1 + beam no nível 0.
    fn search(&self, entries: &[VectorEntry], query: &[f32], top_k: usize) -> Vec<VectorHit> {
        if entries.is_empty() || top_k == 0 {
            return Vec::new();
        }
        let ef = EF_SEARCH_MIN.max(4 * top_k);
        let mut cur = self.entry_point;
        for l in (1..=self.max_level).rev() {
            cur = self.greedy_closest(entries, query, cur, l);
        }
        let mut hits: Vec<VectorHit> = self
            .search_layer(entries, query, cur, 0, ef)
            .into_iter()
            // Mesma regra do linear: dimensão incompatível nunca entra no resultado.
            .filter(|&(idx, _)| entries[idx].embedding.len() == query.len())
            .map(|(idx, _)| {
                let e = &entries[idx];
                VectorHit {
                    score: cosine_similarity(query, &e.embedding),
                    id: e.id.clone(),
                    text: e.text.clone(),
                    metadata: e.metadata.clone(),
                }
            })
            .collect();
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

// ── Backend com cache por revisão ─────────────────────────────────────────────

struct CachedIndex {
    rev: u64,
    entries: Vec<VectorEntry>,
    graph: HnswGraph,
}

/// Cache global de índices, chaveado pelo caminho do Blackboard. Derivado e
/// reconstruível — invalidado com exatidão pela revisão do store.
static CACHE: OnceLock<Mutex<HashMap<String, CachedIndex>>> = OnceLock::new();

/// Backend HNSW (zero-sized; o estado derivado vive no cache global).
pub struct HnswBackend;

/// Instância estática do backend (usada pela seleção em `vector.rs`).
pub(crate) fn hnsw_backend() -> &'static HnswBackend {
    static BACKEND: HnswBackend = HnswBackend;
    &BACKEND
}

impl VectorBackend for HnswBackend {
    fn name(&self) -> &'static str {
        "hnsw"
    }

    fn query(&self, bb: &Blackboard, embedding: &[f32], top_k: usize) -> Vec<VectorHit> {
        let rev = store_revision(bb);
        let key = bb.store_path().to_string_lossy().to_string();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = cache.lock().unwrap();

        let stale = map.get(&key).map(|c| c.rev != rev).unwrap_or(true);
        if stale {
            let entries = load_entries(bb);
            let graph = HnswGraph::build(&entries);
            map.insert(key.clone(), CachedIndex { rev, entries, graph });
        }
        let cached = map.get(&key).expect("índice recém-inserido no cache");
        cached.graph.search(&cached.entries, embedding, top_k)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::LinearBackend;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    /// LCG determinístico — corpus sintético reprodutível sem crate `rand`.
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // 24 bits altos → [0, 1) → [-1, 1)
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
        fn vec(&mut self, dim: usize) -> Vec<f32> {
            (0..dim).map(|_| self.next_f32()).collect()
        }
    }

    fn popula(bb: &Blackboard, n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
        let mut rng = Lcg(seed);
        let mut all = Vec::with_capacity(n);
        for i in 0..n {
            let v = rng.vec(dim);
            bb.vector_insert(&format!("v{:04}", i), &format!("texto {}", i), v.clone(), serde_json::json!(null))
                .unwrap();
            all.push(v);
        }
        all
    }

    #[test]
    fn nivel_deterministico_e_estavel() {
        assert_eq!(deterministic_level("abc"), deterministic_level("abc"));
        // Níveis dentro do teto.
        for i in 0..200 {
            assert!(deterministic_level(&format!("id-{}", i)) <= MAX_LEVEL_CAP);
        }
    }

    #[test]
    fn hnsw_equivale_ao_linear_em_conjunto_pequeno() {
        let bb = temp_bb();
        let vs = popula(&bb, 20, 8, 42);
        // ef_search (≥64) cobre os 20 nós: resultado deve ser idêntico ao exato.
        for q in vs.iter().take(5) {
            let lin: Vec<String> = bb
                .vector_query_with(&LinearBackend, q, 5)
                .into_iter()
                .map(|h| h.id)
                .collect();
            let hnsw: Vec<String> = bb
                .vector_query_with(hnsw_backend(), q, 5)
                .into_iter()
                .map(|h| h.id)
                .collect();
            assert_eq!(lin, hnsw, "com ef ≥ n o HNSW deve ser exato");
        }
    }

    #[test]
    fn hnsw_recall_alto_em_corpus_sintetico() {
        let bb = temp_bb();
        let vs = popula(&bb, 300, 32, 7);
        let mut overlap_total = 0usize;
        let mut esperado_total = 0usize;
        // 10 consultas: vetores do próprio corpus (vizinhança real conhecida).
        for q in vs.iter().step_by(30) {
            let lin: HashSet<String> = bb
                .vector_query_with(&LinearBackend, q, 10)
                .into_iter()
                .map(|h| h.id)
                .collect();
            let hnsw: HashSet<String> = bb
                .vector_query_with(hnsw_backend(), q, 10)
                .into_iter()
                .map(|h| h.id)
                .collect();
            overlap_total += lin.intersection(&hnsw).count();
            esperado_total += lin.len();
        }
        let recall = overlap_total as f64 / esperado_total as f64;
        assert!(
            recall >= 0.9,
            "recall@10 {:.3} abaixo do mínimo 0.9 (R-F-065)",
            recall
        );
    }

    #[test]
    fn hnsw_determinismo_entre_reconstrucoes() {
        // Mesmo conteúdo em dois stores distintos → mesmos resultados
        // (caminhos diferentes forçam duas construções independentes).
        let bb1 = temp_bb();
        let bb2 = temp_bb();
        popula(&bb1, 100, 16, 99);
        popula(&bb2, 100, 16, 99);
        let q = Lcg(123).vec(16);
        let r1: Vec<(String, String)> = bb1
            .vector_query_with(hnsw_backend(), &q, 10)
            .into_iter()
            .map(|h| (h.id, format!("{:.6}", h.score)))
            .collect();
        let r2: Vec<(String, String)> = bb2
            .vector_query_with(hnsw_backend(), &q, 10)
            .into_iter()
            .map(|h| (h.id, format!("{:.6}", h.score)))
            .collect();
        assert_eq!(r1, r2, "mesmo corpus deve produzir exatamente os mesmos hits");
    }

    #[test]
    fn hnsw_cache_invalida_apos_insert() {
        let bb = temp_bb();
        popula(&bb, 50, 8, 5);
        let q = vec![0.5f32; 8];
        let antes = bb.vector_query_with(hnsw_backend(), &q, 3);
        assert!(!antes.iter().any(|h| h.id == "novo"));
        // Vetor idêntico à consulta: obrigatoriamente o 1º após invalidação.
        bb.vector_insert("novo", "recém-inserido", q.clone(), serde_json::json!(null))
            .unwrap();
        let depois = bb.vector_query_with(hnsw_backend(), &q, 3);
        assert_eq!(depois[0].id, "novo", "revisão deve invalidar o cache (staleness)");
    }

    #[test]
    fn hnsw_filtra_dimensao_incompativel() {
        let bb = temp_bb();
        bb.vector_insert("ok", "t", vec![1.0, 0.0], serde_json::json!(null))
            .unwrap();
        bb.vector_insert("ruim", "t", vec![1.0, 0.0, 0.0], serde_json::json!(null))
            .unwrap();
        let hits = bb.vector_query_with(hnsw_backend(), &[1.0, 0.0], 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "ok");
    }

    #[test]
    fn hnsw_store_vazio_e_topk_zero() {
        let bb = temp_bb();
        assert!(bb.vector_query_with(hnsw_backend(), &[1.0], 5).is_empty());
        bb.vector_insert("a", "t", vec![1.0], serde_json::json!(null)).unwrap();
        assert!(bb.vector_query_with(hnsw_backend(), &[1.0], 0).is_empty());
    }
}
