use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Conceito semântico armazenado na memória de longo prazo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticConcept {
    pub name: String,
    pub definition: String,
    pub related: Vec<String>,
}

/// Memória semântica sobre o Blackboard.
pub struct SemanticMemory {
    blackboard: Blackboard,
}

impl SemanticMemory {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Aprende um novo conceito e suas relações.
    pub fn learn_concept(
        &self,
        name: &str,
        definition: &str,
        relations: Vec<(String, String)>,
    ) -> Result<()> {
        let related: Vec<String> = relations.into_iter().map(|(_, target)| target).collect();
        let concept = SemanticConcept {
            name: name.into(),
            definition: definition.into(),
            related,
        };
        let key = format!("{}:concept", name);
        self.blackboard
            .put_tuple("memory:semantic", &key, serde_json::to_value(&concept)?)
    }

    /// Recupera um conceito pelo nome.
    pub fn recall_concept(&self, name: &str) -> Result<Option<SemanticConcept>> {
        let key = format!("{}:concept", name);
        match self.blackboard.get_tuple("memory:semantic", &key) {
            Some(v) => Ok(serde_json::from_value(v).ok()),
            None => Ok(None),
        }
    }

    /// Recupera conceitos relacionados a um nome (usa o campo `related` do conceito).
    pub fn recall_related(&self, name: &str) -> Result<Vec<SemanticConcept>> {
        let mut out = Vec::new();
        if let Some(concept) = self.recall_concept(name)? {
            for rel_name in &concept.related {
                if let Some(c) = self.recall_concept(rel_name)? {
                    out.push(c);
                }
            }
        }
        Ok(out)
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
    fn semantic_memory_aprende_e_recupera_conceito() {
        let bb = temp_bb();
        let mem = SemanticMemory::new(bb);
        mem.learn_concept(
            "rust",
            "Linguagem de programação systems-level.",
            vec![
                ("rust".into(), "cargo".into()),
                ("rust".into(), "ownership".into()),
            ],
        )
        .unwrap();

        let concept = mem.recall_concept("rust").unwrap().unwrap();
        assert_eq!(concept.name, "rust");
        assert!(concept.definition.contains("systems-level"));
        assert!(concept.related.contains(&"cargo".to_string()));
    }

    #[test]
    fn semantic_memory_recupera_conceitos_relacionados() {
        let bb = temp_bb();
        let mem = SemanticMemory::new(bb);
        mem.learn_concept("ownership", "Regra de exclusividade de acesso.", vec![])
            .unwrap();
        mem.learn_concept(
            "rust",
            "Linguagem.",
            vec![("rust".into(), "ownership".into())],
        )
        .unwrap();

        let related = mem.recall_related("rust").unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].name, "ownership");
    }
}
