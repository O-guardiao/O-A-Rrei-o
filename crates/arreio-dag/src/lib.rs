pub mod checkpoint;
pub mod executor;
pub mod score;
pub mod shadow_git;
pub mod todo;
pub mod workspace;

use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

// Re-exporta tipos de contrato do kernel para conveniência de consumidores
pub use arreio_kernel::{ContractViolation, ViolationType};

pub use checkpoint::Checkpoint;
pub use score::NodeScore;
pub use todo::{TodoItem, TodoStatus, TodoStore};
pub use workspace::WorkspaceManager;

// ── Contratos DAC (Deterministic Agent Contract) ──────────────────────────────

/// Modo de falha esperado para um contrato.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureMode {
    pub name: String,
    pub severity: FailureSeverity,
    pub description: String,
}

/// Severidade de uma falha de contrato.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Contrato machine-readable que define expectativas sobre output de um nó DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: String,
    /// Schema JSON do output esperado (ex: {"type": "object", "properties": {...}})
    pub schema: Value,
    /// SLA de latência em milissegundos para execução do nó.
    pub sla_latency_ms: u64,
    /// Taxonomia de falhas esperadas — permite classificar violações.
    pub failure_taxonomy: Vec<FailureMode>,
    /// Campos que devem ser auditados (presença obrigatória no output).
    pub audit_footprint: Vec<String>,
}

// ── Nó do DAG ─────────────────────────────────────────────────────────────────

/// Estados de um nó — análogos aos estados de uma instrução no JCL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Waiting,
    Ready,
    Running,
    Success,
    Failed,
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    pub id: String,
    pub title: String,
    pub depends_on: Vec<String>,
    pub status: NodeStatus,
    pub actor_type: String,
    pub file_target: Option<String>,
    pub instruction: String,
    pub payload: Value,
    // NOVOS CAMPOS — padrão Codex (milestones + acceptance criteria)
    pub validation_cmd: Option<String>, // comando de validação específico do nó
    pub acceptance_criteria: Vec<String>, // critérios de aceite
    pub decision_log: Vec<String>,      // log de decisões tomadas
    // FASE 3 — Multi-Agent
    pub assigned_agent: Option<String>, // agente responsável por executar este nó
    // GAP-001 — Recovery cascade: contador de retries para evitar loops infinitos
    pub retry_count: u32, // número de tentativas de recovery já feitas
    // PVC-Q1.1 — DAC Runtime: IDs dos contracts aplicáveis a este nó
    #[serde(default)]
    pub contracts: Vec<String>,
}

// ── DAG ───────────────────────────────────────────────────────────────────────

/// Grafo Acíclico Dirigido de tarefas, persistido no Blackboard.
/// Cada nó só passa para Ready quando todos seus depends_on estão em Success.
pub struct Dag {
    nodes: Vec<DagNode>,
    blackboard: Blackboard,
}

impl Dag {
    /// Cria um novo DAG a partir de um vetor de nós, validando ciclos.
    pub fn new(nodes: Vec<DagNode>, blackboard: Blackboard) -> Result<Self> {
        let dag = Self { nodes, blackboard };
        if dag.has_cycle() {
            bail!("DAG inválido: ciclo detectado nas dependências");
        }
        dag.persist()?;
        Ok(dag)
    }

    /// Carrega o DAG do Blackboard.
    pub fn load(blackboard: Blackboard) -> Result<Self> {
        let nodes = match blackboard.get_tuple("dag", "nodes") {
            Some(v) => serde_json::from_value(v)?,
            None => vec![],
        };
        Ok(Self { nodes, blackboard })
    }

    /// Nós prontos para execução: Waiting + todos deps em Success.
    pub fn ready_nodes(&self) -> Vec<&DagNode> {
        self.nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Waiting)
            .filter(|n| self.all_deps_succeeded(n))
            .collect()
    }

    // ── Priorização dinâmica (PVC-Q3.1) ───────────────────────────────────────

    /// Define (ou sobrescreve) o score de um nó. O score vive como tupla
    /// `dag::score:{node_id}` no Blackboard — o formato de `DagNode`
    /// permanece intocado para bridges e consumidores externos.
    pub fn set_score(&self, node_id: &str, score: &NodeScore) -> Result<()> {
        if !self.nodes.iter().any(|n| n.id == node_id) {
            bail!("nó não encontrado para score: {}", node_id);
        }
        self.blackboard.put_tuple(
            "dag",
            &format!("score:{}", node_id),
            serde_json::to_value(score)?,
        )
    }

    /// Lê o score de um nó, se existir.
    pub fn score_of(&self, node_id: &str) -> Option<NodeScore> {
        self.blackboard
            .get_tuple("dag", &format!("score:{}", node_id))
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Remove o score de um nó.
    pub fn clear_score(&self, node_id: &str) -> Result<()> {
        self.blackboard
            .delete_tuple("dag", &format!("score:{}", node_id))
    }

    /// Nós prontos ordenados por score composto decrescente (re-scoring
    /// dinâmico: o score é relido do Blackboard a cada chamada, então
    /// alterações entre ciclos mudam a ordem). Nós sem score usam o
    /// `NodeScore::default()` neutro. Empate → ordem por id (determinístico).
    pub fn scored_ready_nodes(&self, now_epoch: u64) -> Vec<(&DagNode, f64)> {
        let mut scored: Vec<(&DagNode, f64)> = self
            .ready_nodes()
            .into_iter()
            .map(|n| {
                let score = self.score_of(&n.id).unwrap_or_default();
                (n, score.composite(now_epoch))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.id.cmp(&b.0.id))
        });
        scored
    }

    fn all_deps_succeeded(&self, node: &DagNode) -> bool {
        node.depends_on.iter().all(|dep_id| {
            self.nodes
                .iter()
                .find(|d| &d.id == dep_id)
                .map(|d| d.status == NodeStatus::Success)
                .unwrap_or(false)
        })
    }

    /// Atualiza o status de um nó e persiste no Blackboard.
    pub fn update_status(&mut self, node_id: &str, status: NodeStatus) -> Result<()> {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            node.status = status;
        } else {
            bail!("nó não encontrado: {}", node_id);
        }
        self.persist()
    }

    /// Retorna true se todos os nós estão em Success.
    pub fn is_complete(&self) -> bool {
        self.nodes.iter().all(|n| n.status == NodeStatus::Success)
    }

    /// Resumo para o Kanban ASCII.
    pub fn summary(&self) -> DagSummary {
        DagSummary {
            todo: self
                .nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Waiting)
                .count(),
            doing: self
                .nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Running)
                .count(),
            done: self
                .nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Success)
                .count(),
            failed: self
                .nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Failed)
                .count(),
            total: self.nodes.len(),
        }
    }

    pub fn nodes(&self) -> &[DagNode] {
        &self.nodes
    }

    pub fn nodes_mut(&mut self) -> &mut [DagNode] {
        &mut self.nodes
    }

    /// Adiciona um novo nó ao DAG, validando ciclos.
    pub fn add_node(&mut self, node: DagNode) -> Result<()> {
        self.nodes.push(node);
        if self.has_cycle() {
            self.nodes.pop();
            bail!("DAG inválido: ciclo detectado após adicionar nó");
        }
        self.persist()
    }

    // ── Detecção de ciclo (DFS) ───────────────────────────────────────────────

    fn has_cycle(&self) -> bool {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();
        for node in &self.nodes {
            if !visited.contains(&node.id) {
                if self.dfs_cycle(&node.id, &mut visited, &mut rec_stack) {
                    return true;
                }
            }
        }
        false
    }

    fn dfs_cycle(
        &self,
        id: &str,
        visited: &mut HashSet<String>,
        stack: &mut HashSet<String>,
    ) -> bool {
        visited.insert(id.to_string());
        stack.insert(id.to_string());
        if let Some(node) = self.nodes.iter().find(|n| n.id == id) {
            for dep in &node.depends_on {
                if !visited.contains(dep) {
                    if self.dfs_cycle(dep, visited, stack) {
                        return true;
                    }
                } else if stack.contains(dep) {
                    return true;
                }
            }
        }
        stack.remove(id);
        false
    }

    pub fn persist(&self) -> Result<()> {
        let v = serde_json::to_value(&self.nodes)?;
        self.blackboard.put_tuple("dag", "nodes", v)
    }
}

pub struct DagSummary {
    pub todo: usize,
    pub doing: usize,
    pub done: usize,
    pub failed: usize,
    pub total: usize,
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_node(id: &str, deps: Vec<&str>) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: id.to_string(),
            depends_on: deps.into_iter().map(String::from).collect(),
            status: NodeStatus::Waiting,
            actor_type: "developer".to_string(),
            file_target: None,
            instruction: String::new(),
            payload: Value::Null,
            validation_cmd: None,
            acceptance_criteria: vec![],
            decision_log: vec![],
            assigned_agent: None,
            retry_count: 0,
            contracts: vec![],
        }
    }

    #[test]
    fn ready_nodes_with_no_deps() {
        let nodes = vec![make_node("a", vec![]), make_node("b", vec!["a"])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        let ready = dag.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
    }

    #[test]
    fn node_becomes_ready_after_dep_succeeds() {
        let nodes = vec![make_node("a", vec![]), make_node("b", vec!["a"])];
        let mut dag = Dag::new(nodes, temp_bb()).unwrap();
        dag.update_status("a", NodeStatus::Success).unwrap();
        let ready: Vec<_> = dag.ready_nodes().iter().map(|n| n.id.clone()).collect();
        assert!(ready.contains(&"b".to_string()));
    }

    #[test]
    fn cycle_detection_rejects_dag() {
        // a → b → a (ciclo)
        let nodes = vec![make_node("a", vec!["b"]), make_node("b", vec!["a"])];
        assert!(Dag::new(nodes, temp_bb()).is_err());
    }

    #[test]
    fn is_complete_when_all_succeed() {
        let nodes = vec![make_node("a", vec![]), make_node("b", vec![])];
        let mut dag = Dag::new(nodes, temp_bb()).unwrap();
        dag.update_status("a", NodeStatus::Success).unwrap();
        dag.update_status("b", NodeStatus::Success).unwrap();
        assert!(dag.is_complete());
    }

    // ── PVC-Q3.1: priorização dinâmica ────────────────────────────────────────

    #[test]
    fn scored_ready_nodes_ordena_por_score_desc() {
        let nodes = vec![
            make_node("baixo", vec![]),
            make_node("alto", vec![]),
            make_node("medio", vec![]),
        ];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        dag.set_score("alto", &NodeScore::new(1.0, 1.0, 0.0, 0.0))
            .unwrap();
        dag.set_score("medio", &NodeScore::new(0.6, 0.6, 0.0, 0.5))
            .unwrap();
        dag.set_score("baixo", &NodeScore::new(0.1, 0.1, 0.0, 1.0))
            .unwrap();

        let ordered: Vec<&str> = dag
            .scored_ready_nodes(0)
            .iter()
            .map(|(n, _)| n.id.as_str())
            .collect();
        assert_eq!(ordered, vec!["alto", "medio", "baixo"]);
    }

    #[test]
    fn no_sem_score_usa_default_neutro() {
        let nodes = vec![make_node("sem-score", vec![]), make_node("urgente", vec![])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        dag.set_score("urgente", &NodeScore::new(1.0, 1.0, 0.5, 0.0))
            .unwrap();
        let ordered: Vec<&str> = dag
            .scored_ready_nodes(0)
            .iter()
            .map(|(n, _)| n.id.as_str())
            .collect();
        assert_eq!(ordered[0], "urgente");
        assert_eq!(ordered[1], "sem-score");
    }

    #[test]
    fn empate_resolve_por_id_deterministico() {
        let nodes = vec![make_node("b", vec![]), make_node("a", vec![])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        let ordered: Vec<&str> = dag
            .scored_ready_nodes(0)
            .iter()
            .map(|(n, _)| n.id.as_str())
            .collect();
        assert_eq!(ordered, vec!["a", "b"]);
    }

    #[test]
    fn rescore_muda_ordem_entre_ciclos() {
        let nodes = vec![make_node("x", vec![]), make_node("y", vec![])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        dag.set_score("x", &NodeScore::new(0.9, 0.9, 0.0, 0.0)).unwrap();
        dag.set_score("y", &NodeScore::new(0.1, 0.1, 0.0, 0.0)).unwrap();
        assert_eq!(dag.scored_ready_nodes(0)[0].0.id, "x");

        // Re-scoring dinâmico: y vira prioridade máxima
        dag.set_score("y", &NodeScore::new(1.0, 1.0, 0.0, 0.0)).unwrap();
        assert_eq!(dag.scored_ready_nodes(0)[0].0.id, "y");
    }

    #[test]
    fn score_persiste_no_blackboard_e_pode_ser_limpo() {
        let nodes = vec![make_node("n", vec![])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        dag.set_score("n", &NodeScore::new(0.7, 0.3, 0.2, 0.1)).unwrap();
        let loaded = dag.score_of("n").unwrap();
        assert!((loaded.urgency - 0.7).abs() < 1e-9);
        dag.clear_score("n").unwrap();
        assert!(dag.score_of("n").is_none());
    }

    #[test]
    fn set_score_em_no_inexistente_falha() {
        let nodes = vec![make_node("n", vec![])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        assert!(dag.set_score("fantasma", &NodeScore::default()).is_err());
    }

    #[test]
    fn deadline_estourado_vence_urgencia_media() {
        let nodes = vec![make_node("deadline", vec![]), make_node("urgente", vec![])];
        let dag = Dag::new(nodes, temp_bb()).unwrap();
        // deadline estourado: pressão 1.0 → 0.3*0.5+0.3*0.5+0.2*1.0+0.1*0+0.1*0.5 = 0.55
        dag.set_score(
            "deadline",
            &NodeScore::default().with_deadline(100),
        )
        .unwrap();
        // urgência média sem deadline: 0.3*0.6+0.3*0.5+0+0+0.05 = 0.38
        dag.set_score("urgente", &NodeScore::new(0.6, 0.5, 0.0, 0.5))
            .unwrap();
        let ordered = dag.scored_ready_nodes(200);
        assert_eq!(ordered[0].0.id, "deadline");
    }

    #[test]
    fn dag_node_backward_compat_no_contracts_field() {
        // Simula JSON antigo (antes de PVC-Q1.1) sem campo contracts
        let json_old = r#"{
            "id": "legacy",
            "title": "legacy",
            "depends_on": [],
            "status": "Waiting",
            "actor_type": "developer",
            "file_target": null,
            "instruction": "",
            "payload": null,
            "validation_cmd": null,
            "acceptance_criteria": [],
            "decision_log": [],
            "assigned_agent": null,
            "retry_count": 0
        }"#;
        let node: DagNode = serde_json::from_str(json_old).unwrap();
        assert!(node.contracts.is_empty());
    }
}
