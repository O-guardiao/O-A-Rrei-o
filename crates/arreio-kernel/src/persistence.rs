use anyhow::Result;
use serde_json::Value;

/// Trait de abstração de storage para o Blackboard.
/// Todas as implementações devem ser thread-safe (`Send + Sync`).
pub trait PersistentStorage: Send + Sync {
    /// Grava ou sobrescreve uma tupla identificada por (categoria, chave).
    fn put(&self, category: &str, key: &str, value: &Value) -> Result<()>;

    /// Recupera uma tupla pelo par (categoria, chave).
    /// Retorna `None` se a chave não existir.
    fn get(&self, category: &str, key: &str) -> Result<Option<Value>>;

    /// Remove uma tupla. Não falha se a chave não existir.
    fn delete(&self, category: &str, key: &str) -> Result<()>;

    /// Busca tuplas cuja categoria começa com `category_prefix`.
    /// Retorna vetor de tuplas (categoria, chave, valor).
    fn search(&self, category_prefix: &str) -> Result<Vec<(String, String, Value)>>;

    /// Executa múltiplas operações de forma atômica (quando suportado pelo backend).
    fn transaction(&self, ops: Vec<StorageOp>) -> Result<()>;
}

/// Operação individual usada dentro de uma transação de storage.
#[derive(Debug, Clone, PartialEq)]
pub enum StorageOp {
    Put {
        category: String,
        key: String,
        value: Value,
    },
    Delete {
        category: String,
        key: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock em memória para exercitar o trait sem depender de I/O.
    struct MockStorage {
        data: Mutex<HashMap<String, Value>>,
    }

    impl MockStorage {
        fn new() -> Self {
            Self {
                data: Mutex::new(HashMap::new()),
            }
        }
    }

    impl PersistentStorage for MockStorage {
        fn put(&self, category: &str, key: &str, value: &Value) -> Result<()> {
            let composite = format!("{}::{}", category, key);
            self.data.lock().unwrap().insert(composite, value.clone());
            Ok(())
        }

        fn get(&self, category: &str, key: &str) -> Result<Option<Value>> {
            let composite = format!("{}::{}", category, key);
            Ok(self.data.lock().unwrap().get(&composite).cloned())
        }

        fn delete(&self, category: &str, key: &str) -> Result<()> {
            let composite = format!("{}::{}", category, key);
            self.data.lock().unwrap().remove(&composite);
            Ok(())
        }

        fn search(&self, category_prefix: &str) -> Result<Vec<(String, String, Value)>> {
            let map = self.data.lock().unwrap();
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
            let mut map = self.data.lock().unwrap();
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
            Ok(())
        }
    }

    #[test]
    fn mock_put_and_get_roundtrip() {
        let store = MockStorage::new();
        store
            .put("fsm", "estado", &serde_json::json!("PLANEJAMENTO"))
            .unwrap();
        let v = store.get("fsm", "estado").unwrap().unwrap();
        assert_eq!(v, serde_json::json!("PLANEJAMENTO"));
    }

    #[test]
    fn mock_get_missing_returns_none() {
        let store = MockStorage::new();
        assert!(store.get("inexistente", "chave").unwrap().is_none());
    }

    #[test]
    fn mock_overwrite_existing() {
        let store = MockStorage::new();
        store.put("cfg", "porta", &serde_json::json!(8080)).unwrap();
        store.put("cfg", "porta", &serde_json::json!(9090)).unwrap();
        assert_eq!(store.get("cfg", "porta").unwrap().unwrap(), 9090);
    }

    #[test]
    fn mock_delete_existing() {
        let store = MockStorage::new();
        store.put("tarefa", "t1", &serde_json::json!("ok")).unwrap();
        store.delete("tarefa", "t1").unwrap();
        assert!(store.get("tarefa", "t1").unwrap().is_none());
    }

    #[test]
    fn mock_delete_missing_ok() {
        let store = MockStorage::new();
        // Não deve panikar ao deletar chave inexistente.
        store.delete("fantasma", "x").unwrap();
    }

    #[test]
    fn mock_search_by_prefix() {
        let store = MockStorage::new();
        store
            .put("metrica", "cpu", &serde_json::json!(0.5))
            .unwrap();
        store
            .put("metrica", "mem", &serde_json::json!(0.8))
            .unwrap();
        store
            .put("log", "erro", &serde_json::json!("falha"))
            .unwrap();
        let res = store.search("metrica").unwrap();
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn mock_search_empty_prefix_returns_all() {
        let store = MockStorage::new();
        store.put("a", "1", &serde_json::json!(1)).unwrap();
        store.put("b", "2", &serde_json::json!(2)).unwrap();
        let res = store.search("").unwrap();
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn mock_search_no_match_returns_empty() {
        let store = MockStorage::new();
        store.put("a", "1", &serde_json::json!(1)).unwrap();
        let res = store.search("zzz").unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn mock_transaction_put_and_delete() {
        let store = MockStorage::new();
        store.put("x", "a", &serde_json::json!(1)).unwrap();
        store.put("x", "b", &serde_json::json!(2)).unwrap();

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
        store.transaction(ops).unwrap();

        assert!(store.get("x", "a").unwrap().is_none());
        assert_eq!(store.get("x", "b").unwrap().unwrap(), 2);
        assert_eq!(store.get("x", "c").unwrap().unwrap(), 3);
    }

    #[test]
    fn mock_transaction_empty_ok() {
        let store = MockStorage::new();
        store.transaction(vec![]).unwrap();
    }

    #[test]
    fn mock_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MockStorage>();
    }
}
