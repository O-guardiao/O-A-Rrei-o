use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::persistence::{PersistentStorage, StorageOp};

/// Implementação do `PersistentStorage` usando arquivo JSON.
/// A persistência é atômica (escreve em `.tmp` e renomeia) para evitar corrupção
/// caso o processo morra durante a escrita.
pub struct JsonBlackboard {
    file_path: PathBuf,
    state: Mutex<HashMap<String, Value>>,
    write_lock: Mutex<()>,
}

impl JsonBlackboard {
    /// Abre ou cria um blackboard JSON no caminho fornecido.
    /// Se o arquivo existir e for um JSON válido, carrega o estado;
    /// caso contrário, inicia vazio.
    pub fn new(file_path: &str) -> Self {
        let path = PathBuf::from(file_path);
        let state = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(raw) => match serde_json::from_str(&raw) {
                    Ok(map) => map,
                    Err(e) => {
                        eprintln!(
                            "[blackboard] ERRO: JSON corrompido em {} — iniciando vazio. erro: {}",
                            path.display(),
                            e
                        );
                        HashMap::new()
                    }
                },
                Err(_) => HashMap::new(),
            }
        } else {
            HashMap::new()
        };
        Self {
            file_path: path,
            state: Mutex::new(state),
            write_lock: Mutex::new(()),
        }
    }

    /// Serializa o estado atual e persiste no disco de forma atômica.
    fn persist(&self) -> Result<()> {
        let map = self.state.lock().unwrap();
        let json = serde_json::to_string_pretty(&*map).context("serializando estado para JSON")?;
        drop(map);
        let _guard = self.write_lock.lock().unwrap();
        let tmp = self.file_path.with_extension("tmp");
        fs::write(&tmp, &json).with_context(|| format!("escrevendo tmp em {}", tmp.display()))?;
        fs::rename(&tmp, &self.file_path).with_context(|| {
            format!(
                "renomeando {} -> {}",
                tmp.display(),
                self.file_path.display()
            )
        })?;
        Ok(())
    }
}

impl PersistentStorage for JsonBlackboard {
    fn put(&self, category: &str, key: &str, value: &Value) -> Result<()> {
        let composite = format!("{}::{}", category, key);
        self.state.lock().unwrap().insert(composite, value.clone());
        self.persist()
    }

    fn get(&self, category: &str, key: &str) -> Result<Option<Value>> {
        let composite = format!("{}::{}", category, key);
        Ok(self.state.lock().unwrap().get(&composite).cloned())
    }

    fn delete(&self, category: &str, key: &str) -> Result<()> {
        let composite = format!("{}::{}", category, key);
        self.state.lock().unwrap().remove(&composite);
        self.persist()
    }

    fn search(&self, category_prefix: &str) -> Result<Vec<(String, String, Value)>> {
        let map = self.state.lock().unwrap();
        let mut results = Vec::new();
        for (k, v) in map.iter() {
            if let Some((cat, key)) = k.split_once("::") {
                if cat.starts_with(category_prefix) {
                    results.push((cat.to_string(), key.to_string(), v.clone()));
                }
            }
        }
        Ok(results)
    }

    fn transaction(&self, ops: Vec<StorageOp>) -> Result<()> {
        let mut map = self.state.lock().unwrap();
        for op in &ops {
            match op {
                StorageOp::Put {
                    category,
                    key,
                    value,
                } => {
                    let composite = format!("{}::{}", category, key);
                    map.insert(composite, value.clone());
                }
                StorageOp::Delete { category, key } => {
                    let composite = format!("{}::{}", category, key);
                    map.remove(&composite);
                }
            }
        }
        drop(map);
        self.persist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{PersistentStorage, StorageOp};
    use std::sync::Arc;
    use std::thread;
    use tempfile::NamedTempFile;

    fn temp_json_bb() -> (JsonBlackboard, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let bb = JsonBlackboard::new(&path);
        (bb, f)
    }

    #[test]
    fn json_put_get_roundtrip() {
        let (bb, _f) = temp_json_bb();
        bb.put("fsm", "estado", &serde_json::json!("EXECUCAO"))
            .unwrap();
        let v = bb.get("fsm", "estado").unwrap().unwrap();
        assert_eq!(v, serde_json::json!("EXECUCAO"));
    }

    #[test]
    fn json_get_missing_returns_none() {
        let (bb, _f) = temp_json_bb();
        assert!(bb.get("inexistente", "chave").unwrap().is_none());
    }

    #[test]
    fn json_overwrite_existing() {
        let (bb, _f) = temp_json_bb();
        bb.put("cfg", "timeout", &serde_json::json!(30)).unwrap();
        bb.put("cfg", "timeout", &serde_json::json!(60)).unwrap();
        assert_eq!(bb.get("cfg", "timeout").unwrap().unwrap(), 60);
    }

    #[test]
    fn json_delete_existing() {
        let (bb, _f) = temp_json_bb();
        bb.put("tarefa", "t1", &serde_json::json!("ok")).unwrap();
        bb.delete("tarefa", "t1").unwrap();
        assert!(bb.get("tarefa", "t1").unwrap().is_none());
    }

    #[test]
    fn json_delete_missing_ok() {
        let (bb, _f) = temp_json_bb();
        bb.delete("fantasma", "x").unwrap();
    }

    #[test]
    fn json_search_by_category_prefix() {
        let (bb, _f) = temp_json_bb();
        bb.put("metrica", "cpu", &serde_json::json!(0.5)).unwrap();
        bb.put("metrica", "mem", &serde_json::json!(0.8)).unwrap();
        bb.put("log", "erro", &serde_json::json!("falha")).unwrap();
        let res = bb.search("metrica").unwrap();
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn json_search_returns_category_key_value() {
        let (bb, _f) = temp_json_bb();
        bb.put("a", "k", &serde_json::json!(42)).unwrap();
        let res = bb.search("a").unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].0, "a");
        assert_eq!(res[0].1, "k");
        assert_eq!(res[0].2, 42);
    }

    #[test]
    fn json_search_no_match_empty() {
        let (bb, _f) = temp_json_bb();
        bb.put("a", "1", &serde_json::json!(1)).unwrap();
        let res = bb.search("zzz").unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn json_transaction_multiple_puts() {
        let (bb, _f) = temp_json_bb();
        let ops = vec![
            StorageOp::Put {
                category: "x".into(),
                key: "a".into(),
                value: serde_json::json!(1),
            },
            StorageOp::Put {
                category: "x".into(),
                key: "b".into(),
                value: serde_json::json!(2),
            },
        ];
        bb.transaction(ops).unwrap();
        assert_eq!(bb.get("x", "a").unwrap().unwrap(), 1);
        assert_eq!(bb.get("x", "b").unwrap().unwrap(), 2);
    }

    #[test]
    fn json_transaction_put_and_delete() {
        let (bb, _f) = temp_json_bb();
        bb.put("x", "a", &serde_json::json!(1)).unwrap();
        bb.put("x", "b", &serde_json::json!(2)).unwrap();

        let ops = vec![
            StorageOp::Put {
                category: "x".into(),
                key: "c".into(),
                value: serde_json::json!(3),
            },
            StorageOp::Delete {
                category: "x".into(),
                key: "a".into(),
            },
        ];
        bb.transaction(ops).unwrap();
        assert!(bb.get("x", "a").unwrap().is_none());
        assert_eq!(bb.get("x", "b").unwrap().unwrap(), 2);
        assert_eq!(bb.get("x", "c").unwrap().unwrap(), 3);
    }

    #[test]
    fn json_transaction_empty_ok() {
        let (bb, _f) = temp_json_bb();
        bb.transaction(vec![]).unwrap();
    }

    #[test]
    fn json_persistence_across_instances() {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap().to_string();

        {
            let bb = JsonBlackboard::new(&path);
            bb.put("persist", "chave", &serde_json::json!("valor"))
                .unwrap();
        }

        {
            let bb = JsonBlackboard::new(&path);
            let v = bb.get("persist", "chave").unwrap().unwrap();
            assert_eq!(v, serde_json::json!("valor"));
        }
    }

    #[test]
    fn json_concurrent_writes() {
        let (bb, _f) = temp_json_bb();
        let bb = Arc::new(bb);
        let n = 20_usize;
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let bb = bb.clone();
            handles.push(thread::spawn(move || {
                bb.put("conc", &i.to_string(), &serde_json::json!(i))
                    .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let res = bb.search("conc").unwrap();
        assert_eq!(res.len(), n);
    }
}
