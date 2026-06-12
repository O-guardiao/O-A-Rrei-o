use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

// ── Persistência ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct BlackboardState {
    tuples: HashMap<String, Value>,           // "cat::key" -> payload
    events: HashMap<String, VecDeque<Event>>, // topic -> fila
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Event {
    pub id: String,
    pub data: Value,
}

// ── Blackboard ────────────────────────────────────────────────────────────────

/// Estado central compartilhado. Atores NÃO se comunicam entre si —
/// apenas lêem e escrevem nesta lousa (padrão HEARSAY-II).
#[derive(Clone)]
pub struct Blackboard {
    inner: Arc<RwLock<BlackboardState>>,
    db_path: PathBuf,
    file_lock: Arc<Mutex<()>>, // serializes tmp→rename to prevent concurrent rename races
}

impl Blackboard {
    /// Abre ou cria o Blackboard no caminho fornecido.
    pub fn open(db_path: &Path) -> Result<Self> {
        let state = if db_path.exists() {
            let raw = fs::read_to_string(db_path)
                .with_context(|| format!("lendo blackboard em {}", db_path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("blackboard corrompido em {}", db_path.display()))?
        } else {
            BlackboardState::default()
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(state)),
            db_path: db_path.to_path_buf(),
            file_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Caminho do arquivo de persistência deste Blackboard. Usado como
    /// identidade estável (ex.: chave de caches derivados — PVC-Q4.2).
    pub fn store_path(&self) -> &Path {
        &self.db_path
    }

    // ── Tuple Space ───────────────────────────────────────────────────────────

    /// Grava ou sobrescreve uma tupla estruturada (chave determinística).
    pub fn put_tuple(&self, category: &str, key: &str, payload: Value) -> Result<()> {
        let composite = format!("{}::{}", category, key);
        self.inner
            .write()
            .unwrap()
            .tuples
            .insert(composite, payload);
        self.persist()
    }

    /// Recupera uma tupla por chave exata. O(log n) via BTreeMap interno.
    pub fn get_tuple(&self, category: &str, key: &str) -> Option<Value> {
        let composite = format!("{}::{}", category, key);
        self.inner.read().unwrap().tuples.get(&composite).cloned()
    }

    /// Remove uma tupla por chave exata.
    pub fn delete_tuple(&self, category: &str, key: &str) -> Result<()> {
        let composite = format!("{}::{}", category, key);
        self.inner.write().unwrap().tuples.remove(&composite);
        self.persist()
    }

    /// Lista tuplas cujo key começa com `prefix`.
    pub fn search_tuples(&self, category: &str, prefix: &str) -> Vec<(String, Value)> {
        let needle = format!("{}::{}", category, prefix);
        self.inner
            .read()
            .unwrap()
            .tuples
            .iter()
            .filter(|(k, _)| k.starts_with(&needle))
            .map(|(k, v)| {
                let key = k.splitn(2, "::").nth(1).unwrap_or("").to_string();
                (key, v.clone())
            })
            .collect()
    }

    // ── Pub/Sub ───────────────────────────────────────────────────────────────

    /// Publica um evento em um tópico (fila FIFO por tópico).
    pub fn publish(&self, topic: &str, data: Value) -> Result<()> {
        let event = Event {
            id: Uuid::new_v4().to_string(),
            data,
        };
        let mut inner = self.inner.write().unwrap();
        inner
            .events
            .entry(topic.to_string())
            .or_default()
            .push_back(event);
        drop(inner);
        self.persist()
    }

    /// Consome o próximo evento do tópico (FIFO, marca como consumido ao remover).
    pub fn next_event(&self, topic: &str) -> Option<Event> {
        let mut inner = self.inner.write().unwrap();
        inner.events.get_mut(topic)?.pop_front()
    }

    /// Verifica se há eventos pendentes no tópico sem consumir.
    pub fn has_event(&self, topic: &str) -> bool {
        self.inner
            .read()
            .unwrap()
            .events
            .get(topic)
            .map(|q| !q.is_empty())
            .unwrap_or(false)
    }

    /// Lista todas as tuplas do blackboard (categoria, chave, valor).
    pub fn list_tuples(&self) -> Vec<(String, String, Value)> {
        self.inner
            .read()
            .unwrap()
            .tuples
            .iter()
            .filter_map(|(k, v)| {
                let (cat, key) = k.split_once("::")?;
                Some((cat.to_string(), key.to_string(), v.clone()))
            })
            .collect()
    }

    // ── Persistência ──────────────────────────────────────────────────────────

    /// Força persistência do estado atual no disco.
    pub fn persist_now(&self) -> Result<()> {
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        let json = {
            let inner = self.inner.read().unwrap();
            serde_json::to_string_pretty(&*inner)?
        };
        // Hold file_lock for the entire write+rename so concurrent threads
        // don't stomp each other's .tmp file.
        let _guard = self.file_lock.lock().unwrap();
        let tmp = self.db_path.with_extension("tmp");
        fs::write(&tmp, &json).with_context(|| format!("escrevendo tmp {}", tmp.display()))?;
        fs::rename(&tmp, &self.db_path).with_context(|| "renomeando blackboard tmp -> db")?;
        Ok(())
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;
    use tempfile::NamedTempFile;

    fn temp_board() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f); // fecha o arquivo mas mantém o path
        Blackboard::open(&path).unwrap()
    }

    #[test]
    fn tuple_put_get_roundtrip() {
        let bb = temp_board();
        bb.put_tuple("fsm", "state", serde_json::json!("PLANNING"))
            .unwrap();
        let v = bb.get_tuple("fsm", "state").unwrap();
        assert_eq!(v, serde_json::json!("PLANNING"));
    }

    #[test]
    fn tuple_overwrite() {
        let bb = temp_board();
        bb.put_tuple("task", "t1", serde_json::json!({"status": "WAITING"}))
            .unwrap();
        bb.put_tuple("task", "t1", serde_json::json!({"status": "SUCCESS"}))
            .unwrap();
        let v = bb.get_tuple("task", "t1").unwrap();
        assert_eq!(v["status"], "SUCCESS");
    }

    #[test]
    fn tuple_search_prefix() {
        let bb = temp_board();
        bb.put_tuple("metrics", "tokens/architect/1", serde_json::json!(42))
            .unwrap();
        bb.put_tuple("metrics", "tokens/developer/1", serde_json::json!(18))
            .unwrap();
        bb.put_tuple("metrics", "other/key", serde_json::json!(0))
            .unwrap();
        let results = bb.search_tuples("metrics", "tokens/");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn event_pub_sub_fifo() {
        let bb = temp_board();
        bb.publish("interrupt", serde_json::json!({"reason": "loop"}))
            .unwrap();
        bb.publish("interrupt", serde_json::json!({"reason": "timeout"}))
            .unwrap();
        let e1 = bb.next_event("interrupt").unwrap();
        assert_eq!(e1.data["reason"], "loop");
        let e2 = bb.next_event("interrupt").unwrap();
        assert_eq!(e2.data["reason"], "timeout");
        assert!(bb.next_event("interrupt").is_none());
    }

    #[test]
    fn concurrent_writes_are_safe() {
        let bb = temp_board();
        let n = 50_usize;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let bb = bb.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    bb.put_tuple("conc", &i.to_string(), serde_json::json!(i))
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // todos os 50 devem estar presentes
        let results = bb.search_tuples("conc", "");
        assert_eq!(results.len(), n);
    }
}
