use crate::problem_space::State;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Operadores tipados que transformam o estado do espaço de problemas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    ReadFile { path: String },
    WriteFile { path: String, content: String },
    ExecuteTest { target: String },
    QueryDoc { query: String },
    Refactor { target: String, instruction: String },
}

impl Operator {
    /// Retorna as precondições do operador como strings descritivas.
    pub fn preconditions(&self) -> Vec<String> {
        match self {
            Operator::ReadFile { path } => vec![format!("file_exists:{}", path)],
            Operator::WriteFile { path, .. } => {
                let dir = std::path::Path::new(path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                vec![format!("directory_exists:{}", dir)]
            }
            Operator::ExecuteTest { target } => vec![format!("test_suite_exists:{}", target)],
            Operator::QueryDoc { .. } => vec!["doc_index_available".to_string()],
            Operator::Refactor { target, .. } => vec![format!("target_exists:{}", target)],
        }
    }

    /// Retorna os efeitos esperados do operador como strings descritivas.
    pub fn effects(&self) -> Vec<String> {
        match self {
            Operator::ReadFile { path } => vec![format!("artifact_read:{}", path)],
            Operator::WriteFile { path, .. } => vec![format!("artifact_written:{}", path)],
            Operator::ExecuteTest { .. } => {
                vec!["tests_executed".to_string(), "metrics_updated".to_string()]
            }
            Operator::QueryDoc { query } => vec![format!("knowledge_acquired:{}", query)],
            Operator::Refactor { target, .. } => vec![format!("code_modified:{}", target)],
        }
    }

    /// Aplica o operador a um estado, retornando um novo estado.
    pub fn apply(&self, state: &State) -> Result<State> {
        let mut new_state = state.clone();
        match self {
            Operator::ReadFile { path } => {
                new_state.artifacts.push(path.clone());
                new_state
                    .description
                    .push_str(&format!("; leu arquivo {}", path));
            }
            Operator::WriteFile { path, content } => {
                if content.is_empty() {
                    bail!("conteúdo vazio para escrita em {}", path);
                }
                if !new_state.artifacts.contains(path) {
                    new_state.artifacts.push(path.clone());
                }
                new_state.description.push_str(&format!(
                    "; escreveu arquivo {} ({} bytes)",
                    path,
                    content.len()
                ));
            }
            Operator::ExecuteTest { target } => {
                new_state
                    .metrics
                    .insert(format!("tests_run_for_{}", target), 1.0);
                new_state
                    .metrics
                    .insert("last_test_passed".to_string(), 1.0);
                new_state
                    .description
                    .push_str(&format!("; executou testes em {}", target));
            }
            Operator::QueryDoc { query } => {
                new_state
                    .description
                    .push_str(&format!("; consultou documentação sobre '{}'", query));
            }
            Operator::Refactor {
                target,
                instruction,
            } => {
                new_state
                    .description
                    .push_str(&format!("; refatorou {}: {}", target, instruction));
                let count = new_state.metrics.get("refactor_count").unwrap_or(&0.0) + 1.0;
                new_state
                    .metrics
                    .insert("refactor_count".to_string(), count);
            }
        }
        Ok(new_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn operator_readfile_modifies_state() {
        let op = Operator::ReadFile {
            path: "src/main.rs".to_string(),
        };
        let state = State {
            description: "inicial".to_string(),
            artifacts: vec![],
            metrics: HashMap::new(),
        };
        let new = op.apply(&state).unwrap();
        assert!(new.artifacts.contains(&"src/main.rs".to_string()));
        assert!(new.description.contains("leu arquivo"));
    }

    #[test]
    fn operator_writefile_modifies_artifacts() {
        let op = Operator::WriteFile {
            path: "output.txt".to_string(),
            content: "hello".to_string(),
        };
        let state = State {
            description: "inicial".to_string(),
            artifacts: vec![],
            metrics: HashMap::new(),
        };
        let new = op.apply(&state).unwrap();
        assert!(new.artifacts.contains(&"output.txt".to_string()));
        assert!(new.description.contains("escreveu arquivo"));
    }

    #[test]
    fn operator_preconditions_and_effects() {
        let op = Operator::ReadFile {
            path: "foo.rs".to_string(),
        };
        assert_eq!(op.preconditions(), vec!["file_exists:foo.rs"]);
        assert_eq!(op.effects(), vec!["artifact_read:foo.rs"]);
    }
}
