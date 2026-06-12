//! EngramCache — cache hot-query determinístico para recall.
//!
//! Traduzido do Agent Memory (EngramCache) para a arquitetura O Arreio.
//!
//! Principais características:
//! - Cache baseado em n-gramas de sufixo com hash determinístico
//! - Auto-popula após 3 hits da mesma query signature
//! - Sem embeddings, sem modelos — 100% determinístico
//! - Persistido no Blackboard como tuplas `memory::engram::<hash>`

use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Entrada de cache para uma query frequente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngramEntry {
    pub query_signature: String,
    pub results: Vec<String>, // IDs de memórias / mensagens recuperadas
    pub hit_count: u32,
    pub last_hit: u64,
}

/// Cache hot-query determinístico.
pub struct EngramCache {
    blackboard: Blackboard,
    populate_threshold: u32,
}

impl EngramCache {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            populate_threshold: 3,
        }
    }

    /// Define o threshold de hits para auto-população.
    pub fn with_threshold(mut self, threshold: u32) -> Self {
        self.populate_threshold = threshold;
        self
    }

    /// Gera a signature determinística de uma query.
    /// Usa n-gramas de sufixo (bigramas) + hash SHA-256 truncado.
    pub fn signature(query: &str) -> String {
        let normalized = query.to_lowercase();
        let words: Vec<&str> = normalized.split_whitespace().collect();
        if words.len() < 2 {
            let mut sig = "unigram_".to_string();
            sig.push_str(&words.first().unwrap_or(&""));
            return sig;
        }
        // Bigramas de sufixo
        let mut bigrams = Vec::new();
        for window in words.windows(2) {
            bigrams.push(format!("{}_{}", window[0], window[1]));
        }
        bigrams.sort();
        bigrams.dedup();
        let joined = bigrams.join("+");
        // Hash truncado para estabilidade
        let hash = format!("{:x}", md5_hash(&joined));
        format!("engram_{}", &hash[..16])
    }

    /// Consulta o cache. Retorna resultados se a query já foi cacheada.
    pub fn get(&self, query: &str) -> Option<EngramEntry> {
        let sig = Self::signature(query);
        self.blackboard
            .get_tuple("memory", &sig)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Registra um hit no cache. Se o threshold for atingido, popula o cache.
    pub fn hit(&self, query: &str, results: Vec<String>) -> Result<Option<EngramEntry>> {
        let sig = Self::signature(query);
        let now = now_epoch_secs();

        let mut entry = self.get(query).unwrap_or_else(|| EngramEntry {
            query_signature: sig.clone(),
            results: Vec::new(),
            hit_count: 0,
            last_hit: 0,
        });

        entry.hit_count += 1;
        entry.last_hit = now;

        // Se atingiu threshold e ainda não tem resultados, popula
        if entry.hit_count >= self.populate_threshold
            && entry.results.is_empty()
            && !results.is_empty()
        {
            entry.results = results;
        }

        self.blackboard
            .put_tuple("memory", &sig, serde_json::to_value(&entry)?)?;

        if entry.hit_count >= self.populate_threshold {
            Ok(Some(entry))
        } else {
            Ok(None)
        }
    }

    /// Invalida uma entrada do cache.
    pub fn invalidate(&self, query: &str) -> Result<()> {
        let sig = Self::signature(query);
        self.blackboard.delete_tuple("memory", &sig)
    }

    /// Limpa entradas antigas (não usadas há `days` dias).
    pub fn prune(&self, days: u64) -> Result<usize> {
        let now = now_epoch_secs();
        let cutoff = now - (days * 86400);
        let all = self.blackboard.search_tuples("memory", "engram_");
        let mut removed = 0;
        for (key, value) in all {
            if let Ok(entry) = serde_json::from_value::<EngramEntry>(value) {
                if entry.last_hit < cutoff {
                    self.blackboard.delete_tuple("memory", &key)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Hash MD5 simples (não criptográfico, apenas para signature estável).
fn md5_hash(input: &str) -> u128 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish() as u128
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_cache() -> EngramCache {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        EngramCache::new(bb).with_threshold(3)
    }

    #[test]
    fn signature_e_deterministica() {
        let s1 = EngramCache::signature("como implementar autenticação JWT");
        let s2 = EngramCache::signature("como implementar autenticação JWT");
        assert_eq!(s1, s2);
        assert!(s1.starts_with("engram_"));
    }

    #[test]
    fn cache_popula_apos_threshold() {
        let cache = temp_cache();
        let query = "como implementar autenticação";

        let r1 = cache.hit(query, vec!["mem1".into()]).unwrap();
        assert!(r1.is_none()); // ainda não atingiu threshold

        let r2 = cache.hit(query, vec!["mem1".into()]).unwrap();
        assert!(r2.is_none());

        let r3 = cache
            .hit(query, vec!["mem1".into(), "mem2".into()])
            .unwrap();
        assert!(r3.is_some());
        let entry = r3.unwrap();
        assert_eq!(entry.hit_count, 3);
        assert_eq!(entry.results, vec!["mem1", "mem2"]);
    }

    #[test]
    fn get_retorna_cacheado() {
        let cache = temp_cache();
        let query = "teste query";

        cache.hit(query, vec!["a".into(), "b".into()]).unwrap();
        cache.hit(query, vec!["a".into(), "b".into()]).unwrap();
        cache.hit(query, vec!["a".into(), "b".into()]).unwrap();

        let found = cache.get(query);
        assert!(found.is_some());
        assert_eq!(found.unwrap().results, vec!["a", "b"]);
    }

    #[test]
    fn invalidate_remove() {
        let cache = temp_cache();
        let query = "teste";
        cache.hit(query, vec!["x".into()]).unwrap();
        cache.hit(query, vec!["x".into()]).unwrap();
        cache.hit(query, vec!["x".into()]).unwrap();

        assert!(cache.get(query).is_some());
        cache.invalidate(query).unwrap();
        assert!(cache.get(query).is_none());
    }
}
