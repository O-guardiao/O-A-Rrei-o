use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::goal::GoalManager;

// ── Tipos ─────────────────────────────────────────────────────────────────────

/// Representa uma configuração no espaço de problemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub description: String,
    pub metadata: Value,
    pub is_goal: bool,
}

impl State {
    pub fn new(description: impl Into<String>, metadata: Value) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.into(),
            metadata,
            is_goal: false,
        }
    }
}

/// Ação que transforma estados no espaço de problemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operator {
    pub id: String,
    pub name: String,
    pub preconditions: Vec<String>,
    pub postconditions: Vec<String>,
    pub cost: u32,
    pub preference: f64,
}

impl Operator {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.into(),
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            cost: 1,
            preference: 0.0,
        }
    }
}

/// Espaço de problemas SOAR: estados, operadores e metas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProblemSpace {
    pub id: String,
    pub name: String,
    pub current_state: State,
    pub goal_states: Vec<State>,
    pub operators: Vec<Operator>,
    pub parent_space_id: Option<String>,
    pub depth: u32,
}

/// Tipos de impasse do SOAR (1982).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImpasseType {
    StateNoChange,
    OperatorTie,
    OperatorConflict,
    OperatorNoChange,
    Rejection,
}

/// Representação de um impasse detectado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impasse {
    pub impasse_type: ImpasseType,
    pub problem_space_id: String,
    pub description: String,
    pub involved_operators: Vec<String>,
    pub created_at: u64,
}

/// Métodos fracos clássicos de IA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeakMethod {
    GenerateAndTest,
    MeansEndsAnalysis,
    HillClimbing,
    BestFirstSearch,
    BreadthFirst,
    DepthFirst,
}

// ── Classificadores de Impasse ────────────────────────────────────────────────

pub trait ImpasseClassifier: Send + Sync {
    fn detect(&self, space: &ProblemSpace, history: &[State]) -> Option<Impasse>;
}

/// Detecta quando o estado não muda por N ciclos consecutivos.
pub struct NoChangeClassifier;

impl ImpasseClassifier for NoChangeClassifier {
    fn detect(&self, space: &ProblemSpace, history: &[State]) -> Option<Impasse> {
        if history.len() >= 3 {
            let last_id = &history[history.len() - 1].id;
            let same = history[history.len() - 3..]
                .iter()
                .all(|s| &s.id == last_id);
            if same {
                return Some(Impasse {
                    impasse_type: ImpasseType::StateNoChange,
                    problem_space_id: space.id.clone(),
                    description: "Estado não mudou por 3 ciclos consecutivos".to_string(),
                    involved_operators: Vec::new(),
                    created_at: now_epoch_secs(),
                });
            }
        }
        None
    }
}

/// Detecta empate entre operadores com preferência similar (delta < epsilon).
pub struct TieClassifier;

const PREFERENCE_EPSILON: f64 = 0.01;

impl ImpasseClassifier for TieClassifier {
    fn detect(&self, space: &ProblemSpace, _history: &[State]) -> Option<Impasse> {
        let applicable: Vec<&Operator> = space
            .operators
            .iter()
            .filter(|op| operator_applicable(op, &space.current_state))
            .collect();

        if applicable.len() < 2 {
            return None;
        }

        let max_pref = applicable
            .iter()
            .map(|op| op.preference)
            .fold(f64::NEG_INFINITY, f64::max);
        let tied: Vec<&Operator> = applicable
            .iter()
            .filter(|op| (op.preference - max_pref).abs() < PREFERENCE_EPSILON)
            .copied()
            .collect();

        if tied.len() >= 2 {
            return Some(Impasse {
                impasse_type: ImpasseType::OperatorTie,
                problem_space_id: space.id.clone(),
                description: format!(
                    "Empate entre {} operadores com preferência ~{}",
                    tied.len(),
                    max_pref
                ),
                involved_operators: tied.iter().map(|o| o.id.clone()).collect(),
                created_at: now_epoch_secs(),
            });
        }
        None
    }
}

/// Detecta conflito de preferências (positivas vs negativas).
pub struct ConflictClassifier;

impl ImpasseClassifier for ConflictClassifier {
    fn detect(&self, space: &ProblemSpace, _history: &[State]) -> Option<Impasse> {
        let applicable: Vec<&Operator> = space
            .operators
            .iter()
            .filter(|op| operator_applicable(op, &space.current_state))
            .collect();

        let has_positive = applicable.iter().any(|op| op.preference > 0.0);
        let has_negative = applicable.iter().any(|op| op.preference < 0.0);

        if has_positive && has_negative {
            return Some(Impasse {
                impasse_type: ImpasseType::OperatorConflict,
                problem_space_id: space.id.clone(),
                description:
                    "Conflito: operadores com preferências positivas e negativas aplicáveis"
                        .to_string(),
                involved_operators: applicable.iter().map(|o| o.id.clone()).collect(),
                created_at: now_epoch_secs(),
            });
        }
        None
    }
}

/// Detecta quando o mesmo operador é aplicado repetidamente sem progresso.
pub struct NoChangeOpClassifier;

impl ImpasseClassifier for NoChangeOpClassifier {
    fn detect(&self, space: &ProblemSpace, history: &[State]) -> Option<Impasse> {
        if history.len() < 3 {
            return None;
        }
        let last = &history[history.len() - 1];
        let op_id = last
            .metadata
            .get("last_operator_id")
            .and_then(|v| v.as_str())?;
        let same = history[history.len() - 3..]
            .iter()
            .all(|s| s.metadata.get("last_operator_id").and_then(|v| v.as_str()) == Some(op_id));
        if same {
            return Some(Impasse {
                impasse_type: ImpasseType::OperatorNoChange,
                problem_space_id: space.id.clone(),
                description: format!("Operador {} aplicado 3x sem mudança de estado", op_id),
                involved_operators: vec![op_id.to_string()],
                created_at: now_epoch_secs(),
            });
        }
        None
    }
}

/// Detecta quando todos os operadores candidatos são rejeitados ou inválidos.
pub struct RejectionClassifier;

impl ImpasseClassifier for RejectionClassifier {
    fn detect(&self, space: &ProblemSpace, _history: &[State]) -> Option<Impasse> {
        if space.operators.is_empty() {
            return None;
        }
        let applicable = space
            .operators
            .iter()
            .any(|op| operator_applicable(op, &space.current_state));
        if !applicable {
            return Some(Impasse {
                impasse_type: ImpasseType::Rejection,
                problem_space_id: space.id.clone(),
                description: "Nenhum operador aplicável no estado atual".to_string(),
                involved_operators: space.operators.iter().map(|o| o.id.clone()).collect(),
                created_at: now_epoch_secs(),
            });
        }
        None
    }
}

fn operator_applicable(op: &Operator, state: &State) -> bool {
    if op.preconditions.is_empty() {
        return true;
    }
    let obj = match state.metadata.as_object() {
        Some(o) => o,
        None => return false,
    };
    op.preconditions.iter().all(|pre| obj.contains_key(pre))
}

// ── Motor de Espaço de Problemas ──────────────────────────────────────────────

/// Motor principal de espaços de problemas com subgoaling universal.
pub struct ProblemSpaceEngine {
    blackboard: Blackboard,
    session_id: String,
    classifiers: Vec<Box<dyn ImpasseClassifier>>,
}

impl ProblemSpaceEngine {
    pub fn new(blackboard: Blackboard, session_id: &str) -> Self {
        Self {
            blackboard,
            session_id: session_id.to_string(),
            classifiers: Vec::new(),
        }
    }

    pub fn with_default_classifiers(mut self) -> Self {
        self.classifiers.push(Box::new(NoChangeClassifier));
        self.classifiers.push(Box::new(TieClassifier));
        self.classifiers.push(Box::new(ConflictClassifier));
        self.classifiers.push(Box::new(NoChangeOpClassifier));
        self.classifiers.push(Box::new(RejectionClassifier));
        self
    }

    fn space_key(&self, space_id: &str) -> String {
        format!("{}:space:{}", self.session_id, space_id)
    }

    fn history_key(&self, space_id: &str) -> String {
        format!("{}:history:{}", self.session_id, space_id)
    }

    /// Cria um novo espaço de problemas.
    pub fn create_space(
        &self,
        name: &str,
        initial_state: State,
        goal_states: Vec<State>,
    ) -> Result<ProblemSpace> {
        let space = ProblemSpace {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            current_state: initial_state,
            goal_states,
            operators: Vec::new(),
            parent_space_id: None,
            depth: 0,
        };
        self.save_space(&space)?;
        self.save_history(&space.id, &[])?;
        Ok(space)
    }

    /// Registra um operador em um espaço.
    pub fn register_operator(&self, space_id: &str, op: Operator) -> Result<()> {
        let mut space = self.get_space_internal(space_id)?;
        space.operators.push(op);
        self.save_space(&space)
    }

    /// Seleciona o melhor operador para o estado atual.
    pub fn select_operator(&self, space_id: &str) -> Result<Option<Operator>> {
        let space = self.get_space_internal(space_id)?;
        let applicable: Vec<&Operator> = space
            .operators
            .iter()
            .filter(|op| operator_applicable(op, &space.current_state))
            .collect();

        if applicable.is_empty() {
            return Ok(None);
        }

        let max_pref = applicable
            .iter()
            .map(|op| op.preference)
            .fold(f64::NEG_INFINITY, f64::max);
        let best: Vec<&Operator> = applicable
            .iter()
            .filter(|op| (op.preference - max_pref).abs() < f64::EPSILON)
            .copied()
            .collect();

        if best.len() > 1 {
            let min_cost = best.iter().map(|op| op.cost).min().unwrap();
            let final_best: Vec<&Operator> = best
                .iter()
                .filter(|op| op.cost == min_cost)
                .copied()
                .collect();
            Ok(final_best.first().cloned().cloned())
        } else {
            Ok(best.first().cloned().cloned())
        }
    }

    /// Aplica um operador, transitando para novo estado.
    pub fn apply_operator(&self, space_id: &str, operator_id: &str) -> Result<State> {
        let mut space = self.get_space_internal(space_id)?;
        let op = space
            .operators
            .iter()
            .find(|o| o.id == operator_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("operador não encontrado: {}", operator_id))?;

        if !operator_applicable(&op, &space.current_state) {
            bail!("precondições do operador não satisfeitas");
        }

        let mut new_metadata = space.current_state.metadata.clone();
        if let Some(obj) = new_metadata.as_object_mut() {
            for post in &op.postconditions {
                obj.insert(post.clone(), Value::Bool(true));
            }
            obj.insert("last_operator_id".to_string(), Value::String(op.id.clone()));
            obj.insert(
                "last_operator_name".to_string(),
                Value::String(op.name.clone()),
            );
        }

        let new_state = State {
            id: Uuid::new_v4().to_string(),
            description: format!("{} [após {}]", space.current_state.description, op.name),
            metadata: new_metadata,
            is_goal: false,
        };

        space.current_state = new_state.clone();
        self.save_space(&space)?;

        let mut history = self.get_history(space_id)?;
        history.push(new_state.clone());
        self.save_history(space_id, &history)?;

        Ok(new_state)
    }

    /// Detecta impasses em um espaço.
    pub fn detect_impasse(&self, space_id: &str, history: &[State]) -> Result<Option<Impasse>> {
        let space = match self.get_space(space_id)? {
            Some(s) => s,
            None => bail!("espaço não encontrado: {}", space_id),
        };

        for classifier in &self.classifiers {
            if let Some(impasse) = classifier.detect(&space, history) {
                return Ok(Some(impasse));
            }
        }
        Ok(None)
    }

    /// Subgoaling universal: cria sub-espaço para resolver um impasse.
    pub fn create_subspace_for_impasse(&self, impasse: &Impasse) -> Result<ProblemSpace> {
        let parent = match self.get_space(&impasse.problem_space_id)? {
            Some(s) => s,
            None => bail!("espaço pai não encontrado: {}", impasse.problem_space_id),
        };

        let initial_state = State {
            id: Uuid::new_v4().to_string(),
            description: format!("Resolver impasse: {:?}", impasse.impasse_type),
            metadata: serde_json::json!({
                "impasse_type": format!("{:?}", impasse.impasse_type),
                "involved_operators": impasse.involved_operators,
            }),
            is_goal: false,
        };

        let goal_state = State {
            id: Uuid::new_v4().to_string(),
            description: "Impasse resolvido".to_string(),
            metadata: serde_json::json!({"resolved": true}),
            is_goal: true,
        };

        let subspace = ProblemSpace {
            id: Uuid::new_v4().to_string(),
            name: format!("subspace-{}-{:?}", parent.name, impasse.impasse_type),
            current_state: initial_state,
            goal_states: vec![goal_state],
            operators: Vec::new(),
            parent_space_id: Some(parent.id.clone()),
            depth: parent.depth + 1,
        };

        self.save_space(&subspace)?;
        self.save_history(&subspace.id, &[])?;
        Ok(subspace)
    }

    /// Lista todos os espaços (incluindo subespaços).
    pub fn list_spaces(&self) -> Result<Vec<ProblemSpace>> {
        let prefix = format!("{}:space:", self.session_id);
        let results = self.blackboard.search_tuples("ps", &prefix);
        let mut spaces = Vec::new();
        for (_, value) in results {
            if let Ok(space) = serde_json::from_value::<ProblemSpace>(value) {
                spaces.push(space);
            }
        }
        Ok(spaces)
    }

    /// Recupera um espaço específico.
    pub fn get_space(&self, space_id: &str) -> Result<Option<ProblemSpace>> {
        match self.blackboard.get_tuple("ps", &self.space_key(space_id)) {
            Some(value) => {
                let space: ProblemSpace = serde_json::from_value(value)?;
                Ok(Some(space))
            }
            None => Ok(None),
        }
    }

    /// Resolve impasse (chamado quando sub-espaço completa).
    pub fn resolve_impasse(
        &self,
        space_id: &str,
        _impasse_type: ImpasseType,
        result: State,
    ) -> Result<()> {
        let mut space = self.get_space_internal(space_id)?;
        space.current_state = result;
        self.save_space(&space)
    }

    /// Verifica se o estado atual é um estado meta.
    pub fn is_goal_reached(&self, space_id: &str) -> Result<bool> {
        let space = self.get_space_internal(space_id)?;
        if space.current_state.is_goal {
            return Ok(true);
        }
        let current_id = &space.current_state.id;
        Ok(space.goal_states.iter().any(|g| &g.id == current_id))
    }

    /// Sugere um método fraco baseado nas características do espaço.
    pub fn suggest_weak_method(&self, space_id: &str) -> Result<Option<WeakMethod>> {
        let space = self.get_space_internal(space_id)?;
        if space.operators.is_empty() {
            return Ok(Some(WeakMethod::GenerateAndTest));
        }
        if space.goal_states.is_empty() {
            return Ok(Some(WeakMethod::HillClimbing));
        }
        if space.depth > 2 {
            return Ok(Some(WeakMethod::DepthFirst));
        }
        if space.operators.len() > 5 {
            return Ok(Some(WeakMethod::BestFirstSearch));
        }
        Ok(Some(WeakMethod::MeansEndsAnalysis))
    }

    // ── Integração com GoalManager ─────────────────────────────────────────────

    /// Cria um espaço de problemas a partir do goal ativo no GoalManager.
    pub fn create_space_from_goal(&self, goal_manager: &GoalManager) -> Result<ProblemSpace> {
        let goal = goal_manager
            .get_goal()
            .ok_or_else(|| anyhow::anyhow!("nenhum goal definido"))?;
        let initial_state = State::new(
            "Estado inicial do goal",
            Value::Object(serde_json::Map::new()),
        );
        let goal_state = State {
            id: Uuid::new_v4().to_string(),
            description: format!("Goal alcançado: {}", goal.text),
            metadata: Value::Object(serde_json::Map::new()),
            is_goal: true,
        };
        self.create_space(&goal.text, initial_state, vec![goal_state])
    }

    /// Cria um subgoal no GoalManager para resolver um impasse.
    pub fn create_subgoal_for_impasse(
        &self,
        goal_manager: &GoalManager,
        impasse: &Impasse,
    ) -> Result<()> {
        let description = format!(
            "Resolver impasse {:?} no espaço {}",
            impasse.impasse_type, impasse.problem_space_id
        );
        goal_manager.add_subgoal(description)
    }

    // ── Helpers internos ───────────────────────────────────────────────────────

    fn get_space_internal(&self, space_id: &str) -> Result<ProblemSpace> {
        match self.get_space(space_id)? {
            Some(s) => Ok(s),
            None => bail!("espaço não encontrado: {}", space_id),
        }
    }

    fn save_space(&self, space: &ProblemSpace) -> Result<()> {
        let value = serde_json::to_value(space)?;
        self.blackboard
            .put_tuple("ps", &self.space_key(&space.id), value)
    }

    fn get_history(&self, space_id: &str) -> Result<Vec<State>> {
        match self.blackboard.get_tuple("ps", &self.history_key(space_id)) {
            Some(value) => {
                let history: Vec<State> = serde_json::from_value(value)?;
                Ok(history)
            }
            None => Ok(Vec::new()),
        }
    }

    fn save_history(&self, space_id: &str, history: &[State]) -> Result<()> {
        let value = serde_json::to_value(history)?;
        self.blackboard
            .put_tuple("ps", &self.history_key(space_id), value)
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    fn temp_engine() -> ProblemSpaceEngine {
        ProblemSpaceEngine::new(temp_bb(), "test-session").with_default_classifiers()
    }

    #[test]
    fn create_space_persists() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({"x": 0}));
        let space = engine
            .create_space("test-space", state.clone(), vec![])
            .unwrap();

        let fetched = engine.get_space(&space.id).unwrap().unwrap();
        assert_eq!(fetched.name, "test-space");
        assert_eq!(fetched.current_state.description, state.description);
        assert_eq!(fetched.depth, 0);
        assert!(fetched.parent_space_id.is_none());
    }

    #[test]
    fn register_and_select_operator() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({"ready": true}));
        let space = engine.create_space("test", state, vec![]).unwrap();

        let mut op = Operator::new("move");
        op.preconditions = vec!["ready".to_string()];
        op.preference = 1.0;
        engine.register_operator(&space.id, op).unwrap();

        let selected = engine.select_operator(&space.id).unwrap();
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().name, "move");
    }

    #[test]
    fn apply_operator_transitions_state() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({"ready": true}));
        let space = engine.create_space("test", state, vec![]).unwrap();

        let mut op = Operator::new("move");
        op.preconditions = vec!["ready".to_string()];
        op.postconditions = vec!["moved".to_string()];
        engine.register_operator(&space.id, op.clone()).unwrap();

        let new_state = engine.apply_operator(&space.id, &op.id).unwrap();
        assert!(new_state.metadata.get("moved").is_some());
        assert_eq!(
            new_state.metadata.get("last_operator_name").unwrap(),
            "move"
        );

        let space = engine.get_space(&space.id).unwrap().unwrap();
        assert_eq!(space.current_state.id, new_state.id);
    }

    #[test]
    fn detect_state_no_change_impasse() {
        let engine = temp_engine();
        let state = State::new("stuck", serde_json::json!({}));
        let space = engine.create_space("test", state.clone(), vec![]).unwrap();

        let history = vec![state.clone(), state.clone(), state.clone()];
        let impasse = engine.detect_impasse(&space.id, &history).unwrap();
        assert!(impasse.is_some());
        assert_eq!(impasse.unwrap().impasse_type, ImpasseType::StateNoChange);
    }

    #[test]
    fn detect_operator_tie_impasse() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({"ready": true}));
        let space = engine.create_space("test", state, vec![]).unwrap();

        let mut op1 = Operator::new("a");
        op1.preference = 1.0;
        op1.preconditions = vec!["ready".to_string()];
        let mut op2 = Operator::new("b");
        op2.preference = 1.0;
        op2.preconditions = vec!["ready".to_string()];
        engine.register_operator(&space.id, op1).unwrap();
        engine.register_operator(&space.id, op2).unwrap();

        let impasse = engine.detect_impasse(&space.id, &[]).unwrap();
        assert!(impasse.is_some());
        assert_eq!(impasse.unwrap().impasse_type, ImpasseType::OperatorTie);
    }

    #[test]
    fn detect_conflict_impasse() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({"ready": true}));
        let space = engine.create_space("test", state, vec![]).unwrap();

        let mut op1 = Operator::new("good");
        op1.preference = 1.0;
        op1.preconditions = vec!["ready".to_string()];
        let mut op2 = Operator::new("bad");
        op2.preference = -1.0;
        op2.preconditions = vec!["ready".to_string()];
        engine.register_operator(&space.id, op1).unwrap();
        engine.register_operator(&space.id, op2).unwrap();

        let impasse = engine.detect_impasse(&space.id, &[]).unwrap();
        assert!(impasse.is_some());
        assert_eq!(impasse.unwrap().impasse_type, ImpasseType::OperatorConflict);
    }

    #[test]
    fn detect_rejection_impasse() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({"missing": false}));
        let space = engine.create_space("test", state, vec![]).unwrap();

        let mut op = Operator::new("needs_key");
        op.preconditions = vec!["key".to_string()];
        engine.register_operator(&space.id, op).unwrap();

        let impasse = engine.detect_impasse(&space.id, &[]).unwrap();
        assert!(impasse.is_some());
        assert_eq!(impasse.unwrap().impasse_type, ImpasseType::Rejection);
    }

    #[test]
    fn universal_subgoaling_creates_subspace() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({}));
        let space = engine.create_space("parent", state, vec![]).unwrap();

        let impasse = Impasse {
            impasse_type: ImpasseType::OperatorTie,
            problem_space_id: space.id.clone(),
            description: "tie".to_string(),
            involved_operators: vec![],
            created_at: 0,
        };

        let subspace = engine.create_subspace_for_impasse(&impasse).unwrap();
        assert_eq!(subspace.parent_space_id, Some(space.id.clone()));
        assert_eq!(subspace.depth, 1);
        assert!(engine.get_space(&subspace.id).unwrap().is_some());
    }

    #[test]
    fn resolve_impasse_updates_parent() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({}));
        let space = engine.create_space("parent", state, vec![]).unwrap();

        let resolved_state = State::new("resolved", serde_json::json!({"fixed": true}));
        engine
            .resolve_impasse(&space.id, ImpasseType::OperatorTie, resolved_state.clone())
            .unwrap();

        let updated = engine.get_space(&space.id).unwrap().unwrap();
        assert_eq!(
            updated.current_state.description,
            resolved_state.description
        );
    }

    #[test]
    fn is_goal_reached_when_state_matches() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({}));
        let goal = State::new("goal", serde_json::json!({}));
        let space = engine
            .create_space("test", state, vec![goal.clone()])
            .unwrap();

        assert!(!engine.is_goal_reached(&space.id).unwrap());

        engine
            .resolve_impasse(&space.id, ImpasseType::StateNoChange, goal.clone())
            .unwrap();
        assert!(engine.is_goal_reached(&space.id).unwrap());
    }

    #[test]
    fn subspace_depth_tracking() {
        let engine = temp_engine();
        let state = State::new("initial", serde_json::json!({}));
        let space = engine.create_space("root", state, vec![]).unwrap();

        let impasse1 = Impasse {
            impasse_type: ImpasseType::OperatorTie,
            problem_space_id: space.id.clone(),
            description: "tie".to_string(),
            involved_operators: vec![],
            created_at: 0,
        };
        let sub1 = engine.create_subspace_for_impasse(&impasse1).unwrap();
        assert_eq!(sub1.depth, 1);

        let impasse2 = Impasse {
            impasse_type: ImpasseType::OperatorConflict,
            problem_space_id: sub1.id.clone(),
            description: "conflict".to_string(),
            involved_operators: vec![],
            created_at: 0,
        };
        let sub2 = engine.create_subspace_for_impasse(&impasse2).unwrap();
        assert_eq!(sub2.depth, 2);
    }

    #[test]
    fn weak_method_selection() {
        let engine = temp_engine();

        // Sem operadores → GenerateAndTest
        let s1 = State::new("s1", serde_json::json!({}));
        let space1 = engine.create_space("empty", s1, vec![]).unwrap();
        assert_eq!(
            engine.suggest_weak_method(&space1.id).unwrap(),
            Some(WeakMethod::GenerateAndTest)
        );

        // Com goal e poucos operadores → MeansEndsAnalysis
        let s2 = State::new("s2", serde_json::json!({}));
        let g2 = State::new("goal", serde_json::json!({}));
        let space2 = engine.create_space("planning", s2, vec![g2]).unwrap();
        let op = Operator::new("step");
        engine.register_operator(&space2.id, op).unwrap();
        assert_eq!(
            engine.suggest_weak_method(&space2.id).unwrap(),
            Some(WeakMethod::MeansEndsAnalysis)
        );
    }
}
