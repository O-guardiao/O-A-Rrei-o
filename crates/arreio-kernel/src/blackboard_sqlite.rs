use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistence::{PersistentStorage, StorageOp};

/// Implementação do `PersistentStorage` usando SQLite.
/// Todas as operações são ACID graças ao suporte nativo de transações do SQLite.
#[derive(Clone)]
pub struct SqliteBlackboard {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBlackboard {
    /// Abre (ou cria) o banco SQLite no caminho fornecido e inicializa o schema.
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("abrindo banco SQLite em {}", db_path))?;
        let bb = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        bb.init_schema()?;
        Ok(bb)
    }

    /// Cria a tabela `tuples` caso ainda não exista.
    pub fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tuples (
                cat        TEXT NOT NULL,
                key        TEXT NOT NULL,
                value      TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (cat, key)
            )",
            [],
        )
        .context("criando schema do SQLite")?;
        Ok(())
    }

    /// Retorna o timestamp atual em segundos desde a época Unix.
    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}

impl PersistentStorage for SqliteBlackboard {
    fn put(&self, category: &str, key: &str, value: &Value) -> Result<()> {
        let json = serde_json::to_string(value).context("serializando valor para JSON")?;
        let now = Self::now_secs();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tuples (cat, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(cat, key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![category, key, json, now],
        )
        .context("put no sqlite")?;
        Ok(())
    }

    fn get(&self, category: &str, key: &str) -> Result<Option<Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT value FROM tuples WHERE cat = ?1 AND key = ?2")
            .context("preparando query get")?;
        let mut rows = stmt.query(params![category, key])?;
        if let Some(row) = rows.next()? {
            let raw: String = row.get(0).context("lendo coluna value")?;
            let val: Value =
                serde_json::from_str(&raw).context("deserializando valor do sqlite")?;
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    fn delete(&self, category: &str, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM tuples WHERE cat = ?1 AND key = ?2",
            params![category, key],
        )
        .context("delete no sqlite")?;
        Ok(())
    }

    fn search(&self, category_prefix: &str) -> Result<Vec<(String, String, Value)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT cat, key, value FROM tuples WHERE cat LIKE ?1")
            .context("preparando query search")?;
        let pattern = format!("{}%", category_prefix);
        let mut rows = stmt.query(params![pattern])?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let cat: String = row.get(0).context("lendo cat")?;
            let key: String = row.get(1).context("lendo key")?;
            let raw: String = row.get(2).context("lendo value")?;
            let val: Value =
                serde_json::from_str(&raw).context("deserializando valor no search")?;
            results.push((cat, key, val));
        }
        Ok(results)
    }

    fn transaction(&self, ops: Vec<StorageOp>) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("iniciando transação sqlite")?;
        for op in &ops {
            match op {
                StorageOp::Put {
                    category,
                    key,
                    value,
                } => {
                    let json =
                        serde_json::to_string(value).context("serializando valor na transação")?;
                    let now = Self::now_secs();
                    tx.execute(
                        "INSERT INTO tuples (cat, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(cat, key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                        params![category, key, json, now],
                    )
                    .context("put dentro de transação")?;
                }
                StorageOp::Delete { category, key } => {
                    tx.execute(
                        "DELETE FROM tuples WHERE cat = ?1 AND key = ?2",
                        params![category, key],
                    )
                    .context("delete dentro de transação")?;
                }
            }
        }
        tx.commit().context("commit da transação sqlite")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{PersistentStorage, StorageOp};
    use std::sync::Arc;
    use std::thread;
    use tempfile::NamedTempFile;

    fn temp_sqlite_bb() -> (SqliteBlackboard, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let bb = SqliteBlackboard::new(&path).unwrap();
        (bb, f)
    }

    #[test]
    fn sqlite_put_get_roundtrip() {
        let (bb, _f) = temp_sqlite_bb();
        bb.put("fsm", "estado", &serde_json::json!("AVALIACAO"))
            .unwrap();
        let v = bb.get("fsm", "estado").unwrap().unwrap();
        assert_eq!(v, serde_json::json!("AVALIACAO"));
    }

    #[test]
    fn sqlite_get_missing_returns_none() {
        let (bb, _f) = temp_sqlite_bb();
        assert!(bb.get("inexistente", "chave").unwrap().is_none());
    }

    #[test]
    fn sqlite_overwrite_existing() {
        let (bb, _f) = temp_sqlite_bb();
        bb.put("cfg", "porta", &serde_json::json!(8080)).unwrap();
        bb.put("cfg", "porta", &serde_json::json!(9090)).unwrap();
        assert_eq!(bb.get("cfg", "porta").unwrap().unwrap(), 9090);
    }

    #[test]
    fn sqlite_delete_existing() {
        let (bb, _f) = temp_sqlite_bb();
        bb.put("tarefa", "t1", &serde_json::json!("ok")).unwrap();
        bb.delete("tarefa", "t1").unwrap();
        assert!(bb.get("tarefa", "t1").unwrap().is_none());
    }

    #[test]
    fn sqlite_delete_missing_ok() {
        let (bb, _f) = temp_sqlite_bb();
        bb.delete("fantasma", "x").unwrap();
    }

    #[test]
    fn sqlite_search_by_category_prefix() {
        let (bb, _f) = temp_sqlite_bb();
        bb.put("metrica", "cpu", &serde_json::json!(0.5)).unwrap();
        bb.put("metrica", "mem", &serde_json::json!(0.8)).unwrap();
        bb.put("log", "erro", &serde_json::json!("falha")).unwrap();
        let res = bb.search("metrica").unwrap();
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn sqlite_search_returns_category_key_value() {
        let (bb, _f) = temp_sqlite_bb();
        bb.put("a", "k", &serde_json::json!(42)).unwrap();
        let res = bb.search("a").unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].0, "a");
        assert_eq!(res[0].1, "k");
        assert_eq!(res[0].2, 42);
    }

    #[test]
    fn sqlite_search_no_match_empty() {
        let (bb, _f) = temp_sqlite_bb();
        bb.put("a", "1", &serde_json::json!(1)).unwrap();
        let res = bb.search("zzz").unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn sqlite_transaction_multiple_puts() {
        let (bb, _f) = temp_sqlite_bb();
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
    fn sqlite_transaction_put_and_delete() {
        let (bb, _f) = temp_sqlite_bb();
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
    fn sqlite_transaction_empty_ok() {
        let (bb, _f) = temp_sqlite_bb();
        bb.transaction(vec![]).unwrap();
    }

    #[test]
    fn sqlite_init_schema_idempotent() {
        let (bb, _f) = temp_sqlite_bb();
        // Chamar init_schema novamente não deve falhar.
        bb.init_schema().unwrap();
        bb.init_schema().unwrap();
    }

    #[test]
    fn sqlite_concurrent_writes() {
        let (bb, _f) = temp_sqlite_bb();
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
