use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Relação entre memórias, persistida no Blackboard com prefixo "relation::".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub subject: String,   // memory_id
    pub predicate: String, // ex: "implements", "resolves", "depends_on"
    pub object: String,    // memory_id
    pub confidence: f32,
}

/// GraphStore sobre o Blackboard — sem SQLite externo.
pub struct GraphStore {
    blackboard: Blackboard,
}

impl GraphStore {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn add_relation(&self, rel: &Relation) -> Result<()> {
        let key = format!("{}::{}::{}::", rel.subject, rel.predicate, rel.object);
        let value = serde_json::to_value(rel)?;
        self.blackboard.put_tuple("relation", &key, value)
    }

    /// Busca relações por subject ou object.
    pub fn query(&self, memory_id: &str) -> Result<Vec<Relation>> {
        let all = self.blackboard.search_tuples("relation", "");
        let mut out = Vec::new();
        for (_, v) in all {
            if let Ok(rel) = serde_json::from_value::<Relation>(v) {
                if rel.subject == memory_id || rel.object == memory_id {
                    out.push(rel);
                }
            }
        }
        Ok(out)
    }

    /// BFS por hops a partir de um memory_id, com decay de score.
    pub fn walk(&self, start_id: &str, max_hops: usize) -> Result<Vec<(String, f32)>> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = vec![(start_id.to_string(), 1.0f32)];
        let mut results = Vec::new();
        visited.insert(start_id.to_string());

        for _ in 0..max_hops {
            let mut next_queue = Vec::new();
            for (id, score) in &queue {
                let rels = self.query(id)?;
                for rel in rels {
                    let neighbor = if &rel.subject == id {
                        &rel.object
                    } else {
                        &rel.subject
                    };
                    if visited.insert(neighbor.clone()) {
                        let new_score = score * rel.confidence * 0.9;
                        results.push((neighbor.clone(), new_score));
                        next_queue.push((neighbor.clone(), new_score));
                    }
                }
            }
            queue = next_queue;
        }

        Ok(results)
    }
}

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

    #[test]
    fn graph_store_add_and_query() {
        let bb = temp_bb();
        let gs = GraphStore::new(bb);
        let rel = Relation {
            subject: "m1".into(),
            predicate: "resolves".into(),
            object: "m2".into(),
            confidence: 0.95,
        };
        gs.add_relation(&rel).unwrap();
        let found = gs.query("m1").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].object, "m2");
    }

    #[test]
    fn graph_walk_bfs() {
        let bb = temp_bb();
        let gs = GraphStore::new(bb);
        gs.add_relation(&Relation {
            subject: "a".into(),
            predicate: "link".into(),
            object: "b".into(),
            confidence: 1.0,
        })
        .unwrap();
        gs.add_relation(&Relation {
            subject: "b".into(),
            predicate: "link".into(),
            object: "c".into(),
            confidence: 1.0,
        })
        .unwrap();
        let walked = gs.walk("a", 2).unwrap();
        assert_eq!(walked.len(), 2);
    }
}
