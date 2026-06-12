use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::impasse::{Impasse, ImpasseType};
use crate::problem_space::State;

/// Limite máximo de profundidade para evitar stack overflow.
const MAX_SUBGOAL_DEPTH: usize = 5;

/// Subgoal gerado automaticamente a partir de um impasse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subgoal {
    pub parent_goal: String,
    pub objective: String,
    pub state: State,
    pub depth: usize,
}

/// Mecanismo de Universal Subgoaling inspirado no SOAR.
pub struct UniversalSubgoaling;

impl UniversalSubgoaling {
    /// Cria um subgoal a partir de um impasse, respeitando o limite de profundidade.
    pub fn create_subgoal(impasse: &Impasse) -> Result<Subgoal> {
        if impasse.candidates.is_empty() {
            bail!("impasse sem candidatos não gera subgoal");
        }

        let depth = Self::infer_depth(&impasse.state);
        if depth >= MAX_SUBGOAL_DEPTH {
            bail!(
                "limite de profundidade de subgoals atingido ({})",
                MAX_SUBGOAL_DEPTH
            );
        }

        let objective = match impasse.impasse_type {
            ImpasseType::StateNoChange => "encontrar operador aplicável".to_string(),
            ImpasseType::OperatorTie => "desempatar operadores".to_string(),
            ImpasseType::OperatorConflict => "resolver conflito de operadores".to_string(),
            ImpasseType::Rejection => "recuperar de rejeição de operadores".to_string(),
        };

        Ok(Subgoal {
            parent_goal: impasse.state.description.clone(),
            objective,
            state: impasse.state.clone(),
            depth: depth + 1,
        })
    }

    /// Resolve um subgoal aplicando heurísticas simples e retorna o estado resultante.
    /// O conhecimento adquirido (novo estado) deve ser propagado ao goal pai (bottom-up).
    pub fn resolve_subgoal(subgoal: &Subgoal) -> Result<State> {
        let mut resolved = subgoal.state.clone();
        resolved
            .description
            .push_str(&format!(" | subgoal_resolved: {}", subgoal.objective));
        // Simula aquisição de conhecimento durante a resolução
        resolved.metrics.insert("subgoal_resolved".to_string(), 1.0);
        Ok(resolved)
    }

    fn infer_depth(state: &State) -> usize {
        state
            .metrics
            .get("subgoal_depth")
            .map(|v| *v as usize)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Operator;
    use std::collections::HashMap;

    fn make_state(depth: f64) -> State {
        State {
            description: "test".to_string(),
            artifacts: vec![],
            metrics: {
                let mut m = HashMap::new();
                m.insert("subgoal_depth".to_string(), depth);
                m
            },
        }
    }

    #[test]
    fn subgoaling_creates_subgoal_for_impasse() {
        let impasse = Impasse {
            impasse_type: ImpasseType::StateNoChange,
            state: make_state(0.0),
            candidates: vec![Operator::QueryDoc {
                query: "help".to_string(),
            }],
        };
        let sub = UniversalSubgoaling::create_subgoal(&impasse).unwrap();
        assert_eq!(sub.depth, 1);
        assert!(sub.objective.contains("encontrar"));
    }

    #[test]
    fn subgoal_resolved_returns_knowledge() {
        let impasse = Impasse {
            impasse_type: ImpasseType::OperatorTie,
            state: make_state(0.0),
            candidates: vec![
                Operator::ReadFile {
                    path: "a".to_string(),
                },
                Operator::ReadFile {
                    path: "b".to_string(),
                },
            ],
        };
        let sub = UniversalSubgoaling::create_subgoal(&impasse).unwrap();
        let resolved = UniversalSubgoaling::resolve_subgoal(&sub).unwrap();
        assert!(resolved.description.contains("subgoal_resolved"));
        assert_eq!(resolved.metrics.get("subgoal_resolved"), Some(&1.0));
    }

    #[test]
    fn subgoaling_depth_limit() {
        let impasse = Impasse {
            impasse_type: ImpasseType::StateNoChange,
            state: make_state(5.0),
            candidates: vec![Operator::QueryDoc {
                query: "help".to_string(),
            }],
        };
        assert!(UniversalSubgoaling::create_subgoal(&impasse).is_err());
    }
}
