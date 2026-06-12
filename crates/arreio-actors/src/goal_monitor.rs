//! GoalMonitor — quinto ator do Arreio (PVC-Q3.1).
//!
//! Compara o progresso real (milestones do Plan × status dos nós DAG)
//! com o progresso esperado (fração do budget consumido) e decide,
//! deterministicamente, entre continuar observando, re-planejar ou
//! escalar para humano. Segue o padrão do Refiner: detecção determinística
//! separada da ação não-determinística (re-planning via LLM é opcional e
//! explícito); relatórios publicados como tuplas no Blackboard.
//!
//! Mapeamento milestone → nó: `plan_to_dag_tasks` preserva `Milestone.id`
//! como `DagNode.id`, então a correspondência é feita por id. Milestones
//! sem nó correspondente são reportadas (nunca silenciadas).

use crate::planner::{Plan, Planner};
use anyhow::Result;
use arreio_dag::{DagNode, NodeStatus};
use arreio_fsm::Fsm;
use arreio_kernel::Blackboard;
use arreio_provider::ProviderClient;
use serde::{Deserialize, Serialize};

/// Configuração de thresholds do monitor (determinística, sem LLM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalMonitorConfig {
    /// Desvio (esperado − real) acima do qual o monitor recomenda re-plan.
    pub deviation_threshold: f64,
    /// Desvio acima do qual o monitor escala para humano.
    pub escalation_threshold: f64,
}

impl Default for GoalMonitorConfig {
    fn default() -> Self {
        Self {
            deviation_threshold: 0.25,
            escalation_threshold: 0.50,
        }
    }
}

/// Ação recomendada pelo monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GoalMonitorAction {
    /// Progresso dentro do esperado.
    ContinueObserving,
    /// Desvio acima do threshold (ou milestone falhada) — re-planejar.
    Replan { reason: String },
    /// Desvio severo — requer decisão humana.
    EscalateToHuman { reason: String },
}

/// Progresso de uma milestone individual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestoneProgress {
    pub milestone_id: String,
    pub title: String,
    /// Status do nó DAG correspondente ("Success", "Failed", "Waiting", ...)
    /// ou "NodeNotFound" se a milestone não tem nó (nunca silenciado).
    pub node_status: String,
    pub completed: bool,
    pub failed: bool,
}

/// Relatório consolidado de uma avaliação do monitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalMonitorReport {
    pub milestones: Vec<MilestoneProgress>,
    pub total_milestones: usize,
    pub completed_milestones: usize,
    pub failed_milestones: usize,
    /// Fração de milestones concluídas [0.0, 1.0].
    pub actual_progress: f64,
    /// Progresso esperado = fração do budget consumido [0.0, 1.0].
    pub expected_progress: f64,
    /// expected − actual (positivo = atrasado).
    pub deviation: f64,
    pub action: GoalMonitorAction,
    pub timestamp: u64,
}

/// Monitor de objetivos sobre o Blackboard.
pub struct GoalMonitor {
    blackboard: Blackboard,
    config: GoalMonitorConfig,
}

impl GoalMonitor {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            config: GoalMonitorConfig::default(),
        }
    }

    pub fn with_config(mut self, config: GoalMonitorConfig) -> Self {
        self.config = config;
        self
    }

    /// Avalia progresso vs. milestones. Determinístico: não chama LLM.
    ///
    /// `budget_used_ratio` é a fração do budget de iterações/custo já
    /// consumida (fornecida pelo chamador, ex.: `IterationBudget` da FSM).
    /// A expectativa é linear: 60% do budget gasto → 60% das milestones.
    pub fn assess(
        &self,
        plan: &Plan,
        nodes: &[DagNode],
        budget_used_ratio: f64,
    ) -> Result<GoalMonitorReport> {
        let mut milestones = Vec::with_capacity(plan.milestones.len());
        let mut completed = 0usize;
        let mut failed = 0usize;

        for ms in &plan.milestones {
            let node = nodes.iter().find(|n| n.id == ms.id);
            let (status_str, is_done, is_failed) = match node {
                Some(n) => (
                    n.status.to_string(),
                    n.status == NodeStatus::Success,
                    n.status == NodeStatus::Failed,
                ),
                None => ("NodeNotFound".to_string(), false, false),
            };
            if is_done {
                completed += 1;
            }
            if is_failed {
                failed += 1;
            }
            milestones.push(MilestoneProgress {
                milestone_id: ms.id.clone(),
                title: ms.title.clone(),
                node_status: status_str,
                completed: is_done,
                failed: is_failed,
            });
        }

        let total = plan.milestones.len();
        let actual = if total > 0 {
            completed as f64 / total as f64
        } else {
            // Plano sem milestones: nada a monitorar, progresso = esperado.
            budget_used_ratio.clamp(0.0, 1.0)
        };
        let expected = budget_used_ratio.clamp(0.0, 1.0);
        let deviation = expected - actual;

        let action = if deviation >= self.config.escalation_threshold {
            GoalMonitorAction::EscalateToHuman {
                reason: format!(
                    "desvio severo: esperado {:.0}%, real {:.0}% ({} de {} milestones; {} falhas)",
                    expected * 100.0,
                    actual * 100.0,
                    completed,
                    total,
                    failed
                ),
            }
        } else if deviation >= self.config.deviation_threshold || failed > 0 {
            GoalMonitorAction::Replan {
                reason: format!(
                    "desvio {:.0}% (threshold {:.0}%); milestones falhadas: {}",
                    deviation * 100.0,
                    self.config.deviation_threshold * 100.0,
                    failed
                ),
            }
        } else {
            GoalMonitorAction::ContinueObserving
        };

        let report = GoalMonitorReport {
            milestones,
            total_milestones: total,
            completed_milestones: completed,
            failed_milestones: failed,
            actual_progress: actual,
            expected_progress: expected,
            deviation,
            action,
            timestamp: now_epoch_secs(),
        };

        // Publica o relatório no Blackboard (padrão do Refiner).
        self.blackboard.put_tuple(
            "goal_monitor",
            "last_report",
            serde_json::to_value(&report)?,
        )?;

        Ok(report)
    }

    /// Dispara o re-planning na FSM: interrompe (→ StrategicRetreat, de onde
    /// Planning é transição válida) e registra o pedido no Blackboard para
    /// auditoria. Não chama LLM.
    pub fn trigger_replan(&self, fsm: &Fsm, reason: &str) -> Result<()> {
        fsm.interrupt()?;
        self.blackboard.put_tuple(
            "goal_monitor",
            "replan_requested",
            serde_json::json!({
                "reason": reason,
                "timestamp": now_epoch_secs(),
            }),
        )
    }

    /// Re-planning automático via LLM (explícito — nunca disparado sem o
    /// chamador decidir). Constrói uma spec enriquecida com o desvio e as
    /// milestones pendentes/falhadas e delega ao Planner, preservando os
    /// contracts do plano original.
    pub fn replan_with(
        &self,
        provider: Box<dyn ProviderClient>,
        model: &str,
        plan: &Plan,
        report: &GoalMonitorReport,
    ) -> Result<Plan> {
        let pending: Vec<String> = report
            .milestones
            .iter()
            .filter(|m| !m.completed)
            .map(|m| {
                format!(
                    "- {} ({}): status {}",
                    m.milestone_id, m.title, m.node_status
                )
            })
            .collect();

        let spec = format!(
            "Re-planejamento solicitado pelo GoalMonitor.\n\
             Objetivo original: {}\n\
             Progresso real: {:.0}% | esperado: {:.0}% | desvio: {:.0}%\n\
             Milestones pendentes ou falhadas:\n{}\n\n\
             Gere um novo plano que conclua o objetivo original priorizando \
             as milestones pendentes e contornando as causas das falhas.",
            plan.goal,
            report.actual_progress * 100.0,
            report.expected_progress * 100.0,
            report.deviation * 100.0,
            pending.join("\n")
        );

        let planner = Planner::new(provider, model);
        let new_plan = planner.plan_with_contracts(&spec, plan.contracts.clone())?;

        // Auditoria do re-planning no Blackboard.
        self.blackboard.put_tuple(
            "goal_monitor",
            "replanned",
            serde_json::json!({
                "original_goal": plan.goal,
                "new_goal": new_plan.goal,
                "new_milestones": new_plan.milestones.len(),
                "deviation": report.deviation,
                "timestamp": now_epoch_secs(),
            }),
        )?;

        Ok(new_plan)
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
    use crate::planner::Milestone;
    use arreio_fsm::AgentState;
    use arreio_provider::MockProvider;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_plan(milestone_ids: &[&str]) -> Plan {
        Plan {
            goal: "entregar feature X".into(),
            non_goals: vec![],
            constraints: vec![],
            milestones: milestone_ids
                .iter()
                .map(|id| Milestone {
                    id: id.to_string(),
                    title: format!("milestone {}", id),
                    description: String::new(),
                    acceptance_criteria: vec![],
                    validation_cmd: None,
                    decision_notes: vec![],
                })
                .collect(),
            contracts: vec![],
        }
    }

    fn make_dag_node(id: &str, status: NodeStatus) -> DagNode {
        DagNode {
            id: id.to_string(),
            title: id.to_string(),
            depends_on: vec![],
            status,
            actor_type: "developer".into(),
            file_target: None,
            instruction: String::new(),
            payload: serde_json::Value::Null,
            validation_cmd: None,
            acceptance_criteria: vec![],
            decision_log: vec![],
            assigned_agent: None,
            retry_count: 0,
            contracts: vec![],
        }
    }

    #[test]
    fn progresso_no_ritmo_continua_observando() {
        let monitor = GoalMonitor::new(temp_bb());
        let plan = make_plan(&["m1", "m2", "m3", "m4"]);
        let nodes = vec![
            make_dag_node("m1", NodeStatus::Success),
            make_dag_node("m2", NodeStatus::Success),
            make_dag_node("m3", NodeStatus::Running),
            make_dag_node("m4", NodeStatus::Waiting),
        ];
        // 50% concluído com 50% do budget → desvio 0
        let report = monitor.assess(&plan, &nodes, 0.5).unwrap();
        assert_eq!(report.action, GoalMonitorAction::ContinueObserving);
        assert!((report.actual_progress - 0.5).abs() < 1e-9);
        assert!((report.deviation).abs() < 1e-9);
    }

    #[test]
    fn desvio_acima_do_threshold_recomenda_replan() {
        let monitor = GoalMonitor::new(temp_bb());
        let plan = make_plan(&["m1", "m2", "m3", "m4"]);
        let nodes = vec![
            make_dag_node("m1", NodeStatus::Success),
            make_dag_node("m2", NodeStatus::Waiting),
            make_dag_node("m3", NodeStatus::Waiting),
            make_dag_node("m4", NodeStatus::Waiting),
        ];
        // 25% concluído com 60% do budget → desvio 35% > threshold 25%
        let report = monitor.assess(&plan, &nodes, 0.6).unwrap();
        assert!(matches!(report.action, GoalMonitorAction::Replan { .. }));
    }

    #[test]
    fn desvio_severo_escala_para_humano() {
        let monitor = GoalMonitor::new(temp_bb());
        let plan = make_plan(&["m1", "m2", "m3", "m4"]);
        let nodes = vec![
            make_dag_node("m1", NodeStatus::Waiting),
            make_dag_node("m2", NodeStatus::Waiting),
            make_dag_node("m3", NodeStatus::Waiting),
            make_dag_node("m4", NodeStatus::Waiting),
        ];
        // 0% concluído com 80% do budget → desvio 80% ≥ 50%
        let report = monitor.assess(&plan, &nodes, 0.8).unwrap();
        assert!(matches!(
            report.action,
            GoalMonitorAction::EscalateToHuman { .. }
        ));
    }

    #[test]
    fn milestone_falhada_forca_replan_mesmo_sem_desvio() {
        let monitor = GoalMonitor::new(temp_bb());
        let plan = make_plan(&["m1", "m2"]);
        let nodes = vec![
            make_dag_node("m1", NodeStatus::Success),
            make_dag_node("m2", NodeStatus::Failed),
        ];
        // 50% concluído com 50% do budget (desvio 0), mas m2 falhou
        let report = monitor.assess(&plan, &nodes, 0.5).unwrap();
        assert_eq!(report.failed_milestones, 1);
        assert!(matches!(report.action, GoalMonitorAction::Replan { .. }));
    }

    #[test]
    fn milestone_sem_no_e_reportada_nao_silenciada() {
        let monitor = GoalMonitor::new(temp_bb());
        let plan = make_plan(&["m1", "orfa"]);
        let nodes = vec![make_dag_node("m1", NodeStatus::Success)];
        let report = monitor.assess(&plan, &nodes, 0.1).unwrap();
        let orfa = report
            .milestones
            .iter()
            .find(|m| m.milestone_id == "orfa")
            .unwrap();
        assert_eq!(orfa.node_status, "NodeNotFound");
    }

    #[test]
    fn thresholds_customizados_sao_respeitados() {
        let config = GoalMonitorConfig {
            deviation_threshold: 0.10,
            escalation_threshold: 0.90,
        };
        let monitor = GoalMonitor::new(temp_bb()).with_config(config);
        let plan = make_plan(&["m1", "m2", "m3", "m4"]);
        let nodes = vec![
            make_dag_node("m1", NodeStatus::Success),
            make_dag_node("m2", NodeStatus::Success),
            make_dag_node("m3", NodeStatus::Success),
            make_dag_node("m4", NodeStatus::Waiting),
        ];
        // desvio 15% — acima de 10%, abaixo de 90% → Replan
        let report = monitor.assess(&plan, &nodes, 0.9).unwrap();
        assert!(matches!(report.action, GoalMonitorAction::Replan { .. }));
    }

    #[test]
    fn relatorio_publicado_no_blackboard() {
        let bb = temp_bb();
        let monitor = GoalMonitor::new(bb.clone());
        let plan = make_plan(&["m1"]);
        let nodes = vec![make_dag_node("m1", NodeStatus::Success)];
        monitor.assess(&plan, &nodes, 0.5).unwrap();
        let saved = bb.get_tuple("goal_monitor", "last_report").unwrap();
        assert_eq!(saved["total_milestones"], 1);
        assert_eq!(saved["completed_milestones"], 1);
    }

    #[test]
    fn trigger_replan_interrompe_fsm_e_audita() {
        let bb = temp_bb();
        let fsm = Fsm::new(bb.clone());
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::Execution).unwrap();

        let monitor = GoalMonitor::new(bb.clone());
        monitor.trigger_replan(&fsm, "desvio de 40%").unwrap();

        // FSM forçada a StrategicRetreat (de onde Planning é válido)
        assert_eq!(fsm.current(), AgentState::StrategicRetreat);
        let audit = bb.get_tuple("goal_monitor", "replan_requested").unwrap();
        assert_eq!(audit["reason"], "desvio de 40%");
        // Replanejamento agora é transição válida
        fsm.transition(AgentState::Planning).unwrap();
    }

    #[test]
    fn replan_with_gera_novo_plano_via_planner() {
        let bb = temp_bb();
        let monitor = GoalMonitor::new(bb.clone());
        let plan = make_plan(&["m1", "m2"]);
        let nodes = vec![
            make_dag_node("m1", NodeStatus::Success),
            make_dag_node("m2", NodeStatus::Failed),
        ];
        let report = monitor.assess(&plan, &nodes, 0.6).unwrap();

        let mock = MockProvider::new(
            r#"{"goal": "entregar feature X (replanejado)", "non_goals": [], "constraints": [],
                "milestones": [{"id": "m2b", "title": "refazer m2", "description": "",
                "acceptance_criteria": [], "validation_cmd": null, "decision_notes": []}]}"#,
        );
        let new_plan = monitor
            .replan_with(Box::new(mock), "mock", &plan, &report)
            .unwrap();
        assert!(new_plan.goal.contains("replanejado"));
        assert_eq!(new_plan.milestones.len(), 1);

        // Auditoria registrada
        let audit = bb.get_tuple("goal_monitor", "replanned").unwrap();
        assert_eq!(audit["new_milestones"], 1);
    }

    #[test]
    fn plano_sem_milestones_nao_dispara_acao() {
        let monitor = GoalMonitor::new(temp_bb());
        let plan = make_plan(&[]);
        let report = monitor.assess(&plan, &[], 0.7).unwrap();
        assert_eq!(report.action, GoalMonitorAction::ContinueObserving);
    }
}
