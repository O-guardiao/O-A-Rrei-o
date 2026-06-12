use anyhow::Result;
use arreio_dag::{DagNode, NodeStatus};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;

use crate::impasse::{Impasse, ImpasseType};
use crate::operator::Operator;
use crate::universal_subgoaling::{Subgoal, UniversalSubgoaling};

/// Estado do espaço de problemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub description: String,
    pub artifacts: Vec<String>,
    pub metrics: HashMap<String, f64>,
}

/// Goal hierárquico no espaço de problemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub objective: String,
    pub priority: u8,
    pub parent: Option<String>,
}

/// Resultado da resolução do espaço de problemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionResult {
    pub success: bool,
    pub final_state: State,
    pub steps_taken: usize,
    pub subgoals_created: usize,
}

/// Engine principal do espaço de problemas.
pub struct ProblemSpace {
    pub states: Vec<State>,
    pub operators: Vec<Operator>,
    pub current_state: State,
    pub goals: Vec<Goal>,
    steps_taken: usize,
    subgoals_created: usize,
    blackboard: Option<Blackboard>,
}

impl ProblemSpace {
    /// Cria um novo espaço de problemas com estado inicial.
    pub fn new(initial_state: State) -> Self {
        Self {
            states: vec![initial_state.clone()],
            operators: vec![],
            current_state: initial_state,
            goals: vec![],
            steps_taken: 0,
            subgoals_created: 0,
            blackboard: None,
        }
    }

    /// Anexa um Blackboard para persistência.
    pub fn with_blackboard(mut self, bb: Blackboard) -> Self {
        self.blackboard = Some(bb);
        self
    }

    /// Adiciona um operador ao espaço.
    pub fn add_operator(&mut self, op: Operator) {
        self.operators.push(op);
    }

    /// Adiciona um goal ao espaço.
    pub fn add_goal(&mut self, goal: Goal) {
        self.goals.push(goal);
    }

    /// Retorna referência ao estado atual.
    pub fn current_state(&self) -> &State {
        &self.current_state
    }

    /// Detecta impasses no estado atual.
    pub fn detect_impasse(&self) -> Option<Impasse> {
        let applicable: Vec<&Operator> = self
            .operators
            .iter()
            .filter(|op| self.is_applicable(op))
            .collect();

        if applicable.is_empty() {
            return Some(Impasse {
                impasse_type: ImpasseType::StateNoChange,
                state: self.current_state.clone(),
                candidates: self.operators.clone(),
            });
        }

        // OperatorConflict: dois WriteFile no mesmo path com conteúdo diferente
        let mut write_targets: HashMap<String, Vec<String>> = HashMap::new();
        for op in &applicable {
            if let Operator::WriteFile { path, content } = op {
                write_targets
                    .entry(path.clone())
                    .or_default()
                    .push(content.clone());
            }
        }
        if write_targets
            .values()
            .any(|contents| contents.len() >= 2 && !contents.windows(2).all(|w| w[0] == w[1]))
        {
            return Some(Impasse {
                impasse_type: ImpasseType::OperatorConflict,
                state: self.current_state.clone(),
                candidates: applicable.into_iter().cloned().collect(),
            });
        }

        // OperatorTie: 2+ operadores com mesma preferência (mesmo número de effects)
        let mut pref_counts: HashMap<usize, usize> = HashMap::new();
        for op in &applicable {
            *pref_counts.entry(op.effects().len()).or_insert(0) += 1;
        }
        if pref_counts.values().any(|&v| v >= 2) {
            return Some(Impasse {
                impasse_type: ImpasseType::OperatorTie,
                state: self.current_state.clone(),
                candidates: applicable.into_iter().cloned().collect(),
            });
        }

        // Rejection: todos os operadores aplicáveis já foram tentados e falharam
        let rejected_count = self
            .current_state
            .metrics
            .get("rejected_ops")
            .unwrap_or(&0.0)
            .clone() as usize;
        if rejected_count >= applicable.len() && !applicable.is_empty() {
            return Some(Impasse {
                impasse_type: ImpasseType::Rejection,
                state: self.current_state.clone(),
                candidates: applicable.into_iter().cloned().collect(),
            });
        }

        None
    }

    fn is_applicable(&self, op: &Operator) -> bool {
        match op {
            Operator::ReadFile { path } => !self
                .current_state
                .metrics
                .contains_key(&format!("missing:{}", path)),
            Operator::WriteFile { content, .. } => !content.is_empty(),
            _ => true,
        }
    }

    /// Loop principal de resolução.
    pub fn resolve(&mut self) -> Result<ResolutionResult> {
        let max_steps = 100;
        for _ in 0..max_steps {
            // Parada antecipada se todos os goals estão satisfeitos
            if !self.goals.is_empty() && self.goals.iter().all(|g| self.goal_satisfied(g)) {
                break;
            }

            if let Some(impasse) = self.detect_impasse() {
                let subgoal = UniversalSubgoaling::create_subgoal(&impasse)?;
                self.subgoals_created += 1;
                let resolved_state = UniversalSubgoaling::resolve_subgoal(&subgoal)?;
                self.current_state =
                    self.merge_subgoal_knowledge(&self.current_state, &resolved_state);
                self.states.push(self.current_state.clone());
                self.steps_taken += 1;
                continue;
            }

            // Tenta aplicar o primeiro operador aplicável
            if let Some(op) = self.select_operator() {
                match op.apply(&self.current_state) {
                    Ok(new_state) => {
                        self.current_state = new_state;
                        self.states.push(self.current_state.clone());
                        self.steps_taken += 1;

                        // Sem goals definidos: modo single-shot, para após 1 operação
                        if self.goals.is_empty() {
                            break;
                        }
                    }
                    Err(_) => {
                        *self
                            .current_state
                            .metrics
                            .entry("rejected_ops".to_string())
                            .or_insert(0.0) += 1.0;
                    }
                }
            } else {
                break;
            }
        }

        Ok(ResolutionResult {
            success: self.goals.iter().all(|g| self.goal_satisfied(g)),
            final_state: self.current_state.clone(),
            steps_taken: self.steps_taken,
            subgoals_created: self.subgoals_created,
        })
    }

    fn select_operator(&self) -> Option<Operator> {
        self.operators
            .iter()
            .filter(|op| self.is_applicable(op))
            .next()
            .cloned()
    }

    /// Faz merge do conhecimento adquirido no subgoal de volta ao estado pai.
    fn merge_subgoal_knowledge(&self, parent: &State, subgoal: &State) -> State {
        let mut merged = parent.clone();
        // Usa apenas o objective para evitar crescimento exponencial da descrição
        merged.description.push_str(&format!(" | subgoal_ok"));
        for (k, v) in &subgoal.metrics {
            merged.metrics.insert(k.clone(), *v);
        }
        for art in &subgoal.artifacts {
            if !merged.artifacts.contains(art) {
                merged.artifacts.push(art.clone());
            }
        }
        // Propaga profundidade
        let depth = subgoal.metrics.get("subgoal_depth").unwrap_or(&0.0);
        merged.metrics.insert("subgoal_depth".to_string(), *depth);
        merged
    }

    fn goal_satisfied(&self, goal: &Goal) -> bool {
        self.current_state.description.contains(&goal.objective)
            || self
                .current_state
                .metrics
                .contains_key(&format!("goal_{}_done", goal.id))
    }

    /// Converte um subgoal em um nó do DAG.
    pub fn subgoal_to_dag_node(&self, subgoal: &Subgoal, depends_on: Vec<String>) -> DagNode {
        DagNode {
            id: format!("subgoal_{}", subgoal.parent_goal.replace(" ", "_")),
            title: subgoal.objective.clone(),
            depends_on,
            status: NodeStatus::Waiting,
            actor_type: "problem_space".to_string(),
            file_target: None,
            instruction: subgoal.objective.clone(),
            payload: serde_json::to_value(subgoal).unwrap_or(Value::Null),
            validation_cmd: None,
            acceptance_criteria: vec![],
            decision_log: vec![],
            assigned_agent: None,
            retry_count: 0,
            contracts: vec![],
        }
    }

    // ── Integração com Blackboard ───────────────────────────────────────────────

    /// Persiste o estado atual no Blackboard sob `problem_space::{goal_id}::state`.
    pub fn put_state(&self, goal_id: &str) -> Result<()> {
        if let Some(bb) = &self.blackboard {
            let key = format!("{}::state", goal_id);
            bb.put_tuple(
                "problem_space",
                &key,
                serde_json::to_value(&self.current_state)?,
            )?;
        }
        Ok(())
    }

    /// Recupera o estado do Blackboard.
    pub fn get_state(&self, goal_id: &str) -> Option<State> {
        self.blackboard
            .as_ref()?
            .get_tuple("problem_space", &format!("{}::state", goal_id))
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Persiste a lista de operadores no Blackboard.
    pub fn put_operators(&self, goal_id: &str) -> Result<()> {
        if let Some(bb) = &self.blackboard {
            let key = format!("{}::operators", goal_id);
            bb.put_tuple(
                "problem_space",
                &key,
                serde_json::to_value(&self.operators)?,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn empty_state() -> State {
        State {
            description: "inicial".to_string(),
            artifacts: vec![],
            metrics: HashMap::new(),
        }
    }

    #[test]
    fn empty_problem_space() {
        let ps = ProblemSpace::new(empty_state());
        assert_eq!(ps.states.len(), 1);
        assert!(ps.operators.is_empty());
        assert!(ps.goals.is_empty());
    }

    #[test]
    fn add_state_and_operator() {
        let mut ps = ProblemSpace::new(empty_state());
        ps.add_operator(Operator::ReadFile {
            path: "a.rs".to_string(),
        });
        assert_eq!(ps.operators.len(), 1);
    }

    #[test]
    fn detect_state_no_change() {
        let ps = ProblemSpace::new(empty_state());
        let impasse = ps.detect_impasse().unwrap();
        assert_eq!(impasse.impasse_type, ImpasseType::StateNoChange);
    }

    #[test]
    fn detect_operator_tie() {
        let mut ps = ProblemSpace::new(empty_state());
        // Dois operadores com mesmo número de effects (1 cada)
        ps.add_operator(Operator::ReadFile {
            path: "a.rs".to_string(),
        });
        ps.add_operator(Operator::ReadFile {
            path: "b.rs".to_string(),
        });
        let impasse = ps.detect_impasse().unwrap();
        assert_eq!(impasse.impasse_type, ImpasseType::OperatorTie);
    }

    #[test]
    fn detect_operator_conflict() {
        let mut ps = ProblemSpace::new(empty_state());
        ps.add_operator(Operator::WriteFile {
            path: "x.txt".to_string(),
            content: "a".to_string(),
        });
        ps.add_operator(Operator::WriteFile {
            path: "x.txt".to_string(),
            content: "b".to_string(),
        });
        let impasse = ps.detect_impasse().unwrap();
        assert_eq!(impasse.impasse_type, ImpasseType::OperatorConflict);
    }

    #[test]
    fn detect_rejection() {
        let mut ps = ProblemSpace::new(empty_state());
        ps.add_operator(Operator::ReadFile {
            path: "x.txt".to_string(),
        });
        // Força rejeição: operador é aplicável mas todas as tentativas falharam
        ps.current_state
            .metrics
            .insert("rejected_ops".to_string(), 1.0);
        let impasse = ps.detect_impasse().unwrap();
        assert_eq!(impasse.impasse_type, ImpasseType::Rejection);
    }

    #[test]
    fn simple_resolution() {
        let mut ps = ProblemSpace::new(empty_state());
        ps.add_operator(Operator::ReadFile {
            path: "a.rs".to_string(),
        });
        let result = ps.resolve().unwrap();
        assert!(result.success);
        assert!(result.steps_taken >= 1);
    }

    #[test]
    fn resolution_with_multiple_subgoals() {
        let mut ps = ProblemSpace::new(empty_state());
        // Cria impasse de tie que gera subgoal
        ps.add_operator(Operator::ReadFile {
            path: "a.rs".to_string(),
        });
        ps.add_operator(Operator::ReadFile {
            path: "b.rs".to_string(),
        });
        let result = ps.resolve().unwrap();
        assert!(result.subgoals_created >= 1);
    }

    #[test]
    fn hierarchical_goal_with_parent() {
        let mut ps = ProblemSpace::new(empty_state());
        let parent = Goal {
            id: "parent".to_string(),
            objective: "objetivo pai".to_string(),
            priority: 1,
            parent: None,
        };
        let child = Goal {
            id: "child".to_string(),
            objective: "objetivo filho".to_string(),
            priority: 2,
            parent: Some("parent".to_string()),
        };
        ps.add_goal(parent);
        ps.add_goal(child);
        assert_eq!(ps.goals[1].parent, Some("parent".to_string()));
    }

    #[test]
    fn blackboard_integration() {
        let bb = temp_bb();
        let mut ps = ProblemSpace::new(empty_state()).with_blackboard(bb);
        ps.add_operator(Operator::ReadFile {
            path: "a.rs".to_string(),
        });
        ps.put_state("g1").unwrap();
        ps.put_operators("g1").unwrap();

        let retrieved = ps.get_state("g1").unwrap();
        assert_eq!(retrieved.description, "inicial");
    }

    #[test]
    fn resolution_result_metrics() {
        let mut ps = ProblemSpace::new(empty_state());
        ps.add_operator(Operator::ExecuteTest {
            target: "crate".to_string(),
        });
        let result = ps.resolve().unwrap();
        assert!(result.success);
        assert_eq!(result.steps_taken, 1);
        assert_eq!(result.subgoals_created, 0);
        assert!(result
            .final_state
            .metrics
            .contains_key("tests_run_for_crate"));
    }

    #[test]
    fn subgoal_to_dag_node_integration() {
        let ps = ProblemSpace::new(empty_state());
        let sub = Subgoal {
            parent_goal: "g1".to_string(),
            objective: "resolver conflito".to_string(),
            state: empty_state(),
            depth: 1,
        };
        let node = ps.subgoal_to_dag_node(&sub, vec!["dep1".to_string()]);
        assert_eq!(node.status, NodeStatus::Waiting);
        assert!(node.depends_on.contains(&"dep1".to_string()));
    }
}
