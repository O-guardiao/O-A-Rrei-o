use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Blackboard, VarietyEngine};

// ═══════════════════════════════════════════════════════════════════════════════
// Tipos compartilhados do VSM
// ═══════════════════════════════════════════════════════════════════════════════

/// Tarefa operacional executada pelo System 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub payload: Value,
}

/// Resultado da execução de uma tarefa.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskResult {
    pub task_id: String,
    pub success: bool,
    pub output: Value,
    pub duration_ms: u64,
}

/// Estado operacional agregado do System 1.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OperationalStatus {
    pub tasks_executed: usize,
    pub tasks_failed: usize,
    pub avg_duration_ms: u64,
    pub last_task_id: Option<String>,
}

/// Requisição de recurso para coordenação.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequest {
    pub task_id: String,
    pub resource_type: String,
    pub amount: u64,
    pub priority: u32, // maior = mais prioritário
}

/// Alocação de recurso resultante.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Allocation {
    pub task_id: String,
    pub resource_type: String,
    pub granted: u64,
    pub reason: String,
}

/// Operação agendável.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub depends_on: Vec<String>,
    pub estimated_duration_ms: u64,
}

/// Cronograma de operações.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Schedule {
    pub order: Vec<String>,
    pub estimated_total_ms: u64,
}

/// Restrição a ser imposta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    pub name: String,
    pub max_value: u64,
    pub current_value: u64,
}

/// Relatório de auditoria.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditReport {
    pub violations: Vec<String>,
    pub checked: usize,
}

/// Relatório de performance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerformanceReport {
    pub throughput: f64,  // tarefas / segundo
    pub error_rate: f64,  // 0.0 - 1.0
    pub utilization: f64, // 0.0 - 1.0
}

/// Mudança detectada no ambiente.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvironmentalChange {
    pub source: String,
    pub description: String,
    pub severity: f64, // 0.0 - 1.0
}

/// Previsão de necessidades futuras.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Forecast {
    pub resource_type: String,
    pub predicted_demand: u64,
    pub horizon_secs: u64,
}

/// Oportunidade identificada.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Opportunity {
    pub description: String,
    pub expected_gain: f64,
}

/// Pontuação de alinhamento com política.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlignmentScore {
    pub goal: String,
    pub score: f64, // 0.0 - 1.0
}

/// Pedido de desvio da política.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviationRequest {
    pub reason: String,
    pub requested_action: String,
    pub risk_level: f64, // 0.0 - 1.0
}

/// Autorização de desvio.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Authorization {
    pub approved: bool,
    pub conditions: Vec<String>,
}

/// Diretiva de política.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolicyDirective {
    pub id: String,
    pub text: String,
    pub mandatory: bool,
}

// ═══════════════════════════════════════════════════════════════════════════════
// System 1 — Operações
// ═══════════════════════════════════════════════════════════════════════════════

/// System 1: execução direta de tarefas.
pub struct System1Operations;

impl System1Operations {
    pub fn execute_task(&self, task: &Task) -> Result<TaskResult> {
        let start = now_ms();
        // Simulação determinística: sucesso a menos que o payload indique falha.
        let success = task
            .payload
            .get("should_fail")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            == false;

        let output = if success {
            serde_json::json!({ "status": "ok", "task": task.name })
        } else {
            serde_json::json!({ "status": "failed", "task": task.name })
        };

        Ok(TaskResult {
            task_id: task.id.clone(),
            success,
            output,
            duration_ms: now_ms().saturating_sub(start).max(1),
        })
    }

    pub fn get_operational_status(&self, history: &[TaskResult]) -> OperationalStatus {
        let total = history.len();
        let failed = history.iter().filter(|r| !r.success).count();
        let avg = if total > 0 {
            history.iter().map(|r| r.duration_ms).sum::<u64>() / total as u64
        } else {
            0
        };
        OperationalStatus {
            tasks_executed: total,
            tasks_failed: failed,
            avg_duration_ms: avg,
            last_task_id: history.last().map(|r| r.task_id.clone()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// System 2 — Coordenação
// ═══════════════════════════════════════════════════════════════════════════════

/// System 2: resolve conflitos entre unidades operacionais.
pub struct System2Coordination;

impl System2Coordination {
    /// Aloca recursos por prioridade descrescente; se não houver capacidade,
    /// concede proporção restante.
    pub fn resolve_resource_conflict(
        &self,
        requests: &[ResourceRequest],
        capacity: u64,
    ) -> Vec<Allocation> {
        let mut sorted: Vec<_> = requests.iter().collect();
        sorted.sort_by_key(|r| std::cmp::Reverse(r.priority));

        let mut remaining = capacity;
        let mut out = Vec::new();

        for req in sorted {
            let grant = req.amount.min(remaining);
            remaining = remaining.saturating_sub(grant);
            out.push(Allocation {
                task_id: req.task_id.clone(),
                resource_type: req.resource_type.clone(),
                granted: grant,
                reason: if grant == req.amount {
                    "full".to_string()
                } else if grant > 0 {
                    "partial".to_string()
                } else {
                    "denied".to_string()
                },
            });
        }
        out
    }

    /// Agendamento topológico simples baseado em dependências.
    pub fn schedule_operations(&self, ops: &[Operation]) -> Schedule {
        let mut visited = HashMap::new();
        let mut order = Vec::new();
        let mut est_total = 0u64;

        for op in ops {
            if !visited.contains_key(&op.id) {
                Self::dfs(op, ops, &mut visited, &mut order, &mut est_total);
            }
        }

        Schedule {
            order,
            estimated_total_ms: est_total,
        }
    }

    fn dfs(
        current: &Operation,
        all: &[Operation],
        visited: &mut HashMap<String, bool>,
        order: &mut Vec<String>,
        acc_ms: &mut u64,
    ) {
        if visited.get(&current.id) == Some(&true) {
            return;
        }
        visited.insert(current.id.clone(), true);

        for dep in &current.depends_on {
            if let Some(next) = all.iter().find(|o| &o.id == dep) {
                Self::dfs(next, all, visited, order, acc_ms);
            }
        }
        order.push(current.id.clone());
        *acc_ms += current.estimated_duration_ms;
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// System 3 — Controle
// ═══════════════════════════════════════════════════════════════════════════════

/// System 3: monitora e controla System 1 + 2.
pub struct System3Control;

impl System3Control {
    /// Atenua contexto excessivo para limitar sobrecarga cognitiva (S3).
    pub fn attenuate_context(&self, context: Vec<String>, max_variety: usize) -> Vec<String> {
        VarietyEngine::attenuate_variety(context, max_variety)
    }
}

impl System3Control {
    pub fn audit_operations(&self, results: &[TaskResult]) -> AuditReport {
        let mut violations = Vec::new();
        for r in results.iter().filter(|r| !r.success) {
            violations.push(format!("task {} failed", r.task_id));
        }
        AuditReport {
            violations,
            checked: results.len(),
        }
    }

    pub fn enforce_constraints(&self, constraints: &[Constraint]) -> Result<()> {
        for c in constraints {
            if c.current_value > c.max_value {
                anyhow::bail!(
                    "constraint '{}' violated: {} > {}",
                    c.name,
                    c.current_value,
                    c.max_value
                );
            }
        }
        Ok(())
    }

    pub fn generate_performance_report(&self, status: &OperationalStatus) -> PerformanceReport {
        let total = status.tasks_executed as f64;
        let failed = status.tasks_failed as f64;
        let error_rate = if total > 0.0 { failed / total } else { 0.0 };
        let throughput = if status.avg_duration_ms > 0 {
            1000.0 / status.avg_duration_ms as f64
        } else {
            0.0
        };
        let utilization = if total > 0.0 { 1.0 - error_rate } else { 0.0 };
        PerformanceReport {
            throughput,
            error_rate,
            utilization,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// System 4 — Inteligência
// ═══════════════════════════════════════════════════════════════════════════════

/// System 4: explora ambiente externo e antecipa necessidades.
pub struct System4Intelligence {
    blackboard: Blackboard,
}

impl System4Intelligence {
    /// Amplia diversidade de estratégias quando há insuficiência (S4).
    pub fn amplify_strategies(&self, strategies: Vec<String>, min_variety: usize) -> Vec<String> {
        VarietyEngine::amplify_variety(strategies, min_variety)
    }
}

impl System4Intelligence {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Escaneia o blackboard buscando anomalias ou mudanças.
    pub fn scan_environment(&self) -> Vec<EnvironmentalChange> {
        let mut changes = Vec::new();
        // Verifica eventos no tópico "interrupt"
        if self.blackboard.has_event("interrupt") {
            changes.push(EnvironmentalChange {
                source: "blackboard".to_string(),
                description: "evento de interrupção pendente".to_string(),
                severity: 0.8,
            });
        }
        // Verifica estado FSM em StrategicRetreat
        if let Some(v) = self.blackboard.get_tuple("fsm", "current") {
            if v.as_str() == Some("StrategicRetreat") {
                changes.push(EnvironmentalChange {
                    source: "fsm".to_string(),
                    description: "máquina de estados em retirada estratégica".to_string(),
                    severity: 0.9,
                });
            }
        }
        changes
    }

    /// Previsão simples: se houver muitas falhas, prevê aumento de demanda.
    pub fn forecast_needs(&self, history: &[TaskResult], horizon: Duration) -> Vec<Forecast> {
        let fail_count = history.iter().filter(|r| !r.success).count() as u64;
        let horizon_secs = horizon.as_secs().max(1);
        vec![Forecast {
            resource_type: "retry_capacity".to_string(),
            predicted_demand: fail_count * 2,
            horizon_secs,
        }]
    }

    /// Identifica oportunidades com base no histórico.
    pub fn identify_opportunities(&self, history: &[TaskResult]) -> Vec<Opportunity> {
        let fail_count = history.iter().filter(|r| !r.success).count();
        if fail_count > 2 {
            vec![Opportunity {
                description: "adicionar cache ou retry automático".to_string(),
                expected_gain: fail_count as f64 * 0.15,
            }]
        } else {
            vec![]
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// System 5 — Política
// ═══════════════════════════════════════════════════════════════════════════════

/// System 5: define identidade e propósito.
pub struct System5Policy {
    directives: Vec<PolicyDirective>,
}

impl System5Policy {
    pub fn new() -> Self {
        Self {
            directives: vec![
                PolicyDirective {
                    id: "p1".to_string(),
                    text: "nunca executar comandos destrutivos sem autorização".to_string(),
                    mandatory: true,
                },
                PolicyDirective {
                    id: "p2".to_string(),
                    text: "priorizar correção sobre execução quando falha > 3".to_string(),
                    mandatory: true,
                },
                PolicyDirective {
                    id: "p3".to_string(),
                    text: "manter auditoria completa de todas as operações".to_string(),
                    mandatory: false,
                },
            ],
        }
    }

    pub fn evaluate_goal_alignment(&self, goal: &str) -> AlignmentScore {
        // Heurística simples: palavras-chave positivas aumentam score.
        let keywords = ["seguro", "auditável", "correto", "eficiente"];
        let hits = keywords.iter().filter(|k| goal.contains(*k)).count() as f64;
        let score = (hits / keywords.len() as f64).clamp(0.0, 1.0);
        AlignmentScore {
            goal: goal.to_string(),
            score: score.max(0.1), // mínimo de 0.1 para não ser zero inutilizável
        }
    }

    pub fn authorize_deviation(&self, deviation: &DeviationRequest) -> Authorization {
        if deviation.risk_level < 0.3 {
            Authorization {
                approved: true,
                conditions: vec!["monitorar métricas".to_string()],
            }
        } else if deviation.risk_level < 0.7 {
            Authorization {
                approved: true,
                conditions: vec![
                    "aprovação do operador".to_string(),
                    "rollback preparado".to_string(),
                ],
            }
        } else {
            Authorization {
                approved: false,
                conditions: vec!["risco excessivo".to_string()],
            }
        }
    }

    pub fn get_policy_directives(&self) -> Vec<PolicyDirective> {
        self.directives.clone()
    }
}

impl Default for System5Policy {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// VSM Controller
// ═══════════════════════════════════════════════════════════════════════════════

/// Controlador VSM completo — integra os 5 sistemas de Beer.
pub struct VsmController {
    blackboard: Blackboard,
    system1: System1Operations,
    system2: System2Coordination,
    system3: System3Control,
    system4: System4Intelligence,
    system5: System5Policy,
    variety_engine: VarietyEngine,
}

impl VsmController {
    pub fn new(blackboard: Blackboard) -> Self {
        let system4 = System4Intelligence::new(blackboard.clone());
        Self {
            blackboard,
            system1: System1Operations,
            system2: System2Coordination,
            system3: System3Control,
            system4,
            system5: System5Policy::new(),
            variety_engine: VarietyEngine::default(),
        }
    }

    pub fn variety_engine(&self) -> &VarietyEngine {
        &self.variety_engine
    }

    pub fn system1(&self) -> &System1Operations {
        &self.system1
    }
    pub fn system2(&self) -> &System2Coordination {
        &self.system2
    }
    pub fn system3(&self) -> &System3Control {
        &self.system3
    }
    pub fn system4(&self) -> &System4Intelligence {
        &self.system4
    }
    pub fn system5(&self) -> &System5Policy {
        &self.system5
    }

    /// Persiste o estado operacional no blackboard.
    pub fn persist_status(&self, status: &OperationalStatus) -> Result<()> {
        self.blackboard
            .put_tuple("vsm", "operational_status", serde_json::to_value(status)?)
            .context("persistindo operational_status")
    }

    /// Carrega o estado operacional do blackboard.
    pub fn load_status(&self) -> Option<OperationalStatus> {
        self.blackboard
            .get_tuple("vsm", "operational_status")
            .and_then(|v| serde_json::from_value(v).ok())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Utilitários
// ═══════════════════════════════════════════════════════════════════════════════

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ═══════════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_board() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    #[test]
    fn system1_executes_task() {
        let s1 = System1Operations;
        let task = Task {
            id: "t1".to_string(),
            name: "test".to_string(),
            payload: serde_json::json!({}),
        };
        let res = s1.execute_task(&task).unwrap();
        assert_eq!(res.task_id, "t1");
        assert!(res.success);
    }

    #[test]
    fn system1_detects_failure() {
        let s1 = System1Operations;
        let task = Task {
            id: "t2".to_string(),
            name: "fail".to_string(),
            payload: serde_json::json!({ "should_fail": true }),
        };
        let res = s1.execute_task(&task).unwrap();
        assert!(!res.success);
    }

    #[test]
    fn system2_resolves_conflict() {
        let s2 = System2Coordination;
        let requests = vec![
            ResourceRequest {
                task_id: "a".to_string(),
                resource_type: "cpu".to_string(),
                amount: 80,
                priority: 10,
            },
            ResourceRequest {
                task_id: "b".to_string(),
                resource_type: "cpu".to_string(),
                amount: 40,
                priority: 5,
            },
        ];
        let allocations = s2.resolve_resource_conflict(&requests, 100);
        let a = allocations.iter().find(|x| x.task_id == "a").unwrap();
        let b = allocations.iter().find(|x| x.task_id == "b").unwrap();
        assert_eq!(a.granted, 80);
        assert_eq!(b.granted, 20); // parcial
        assert_eq!(b.reason, "partial");
    }

    #[test]
    fn system3_audits_operations() {
        let s3 = System3Control;
        let results = vec![
            TaskResult {
                task_id: "t1".to_string(),
                success: true,
                output: Value::Null,
                duration_ms: 10,
            },
            TaskResult {
                task_id: "t2".to_string(),
                success: false,
                output: Value::Null,
                duration_ms: 5,
            },
        ];
        let report = s3.audit_operations(&results);
        assert_eq!(report.checked, 2);
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].contains("t2"));
    }

    #[test]
    fn system4_scans_environment() {
        let bb = temp_board();
        let s4 = System4Intelligence::new(bb.clone());
        // Sem eventos, lista vazia
        assert!(s4.scan_environment().is_empty());

        bb.publish("interrupt", serde_json::json!({ "reason": "loop" }))
            .unwrap();
        let changes = s4.scan_environment();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].severity, 0.8);
    }

    #[test]
    fn system5_evaluates_alignment() {
        let s5 = System5Policy::new();
        let score = s5.evaluate_goal_alignment("garantir sistema seguro e auditável");
        assert!(score.score > 0.4);
        let low = s5.evaluate_goal_alignment("fazer qualquer coisa");
        assert!(low.score < 0.4);
    }

    #[test]
    fn vsm_controller_runs_all_systems() {
        let bb = temp_board();
        let ctrl = VsmController::new(bb);

        // Executa tarefa via S1
        let task = Task {
            id: "t1".to_string(),
            name: "demo".to_string(),
            payload: serde_json::json!({}),
        };
        let res = ctrl.system1().execute_task(&task).unwrap();
        assert!(res.success);

        // S5 retorna diretivas
        let dirs = ctrl.system5().get_policy_directives();
        assert!(!dirs.is_empty());
        assert!(dirs.iter().any(|d| d.mandatory));
    }

    #[test]
    fn system3_attenuates_context() {
        let bb = temp_board();
        let ctrl = VsmController::new(bb);
        let context = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let reduced = ctrl.system3().attenuate_context(context, 2);
        assert_eq!(reduced.len(), 2);
        assert_eq!(reduced, vec!["a", "b"]);
    }

    #[test]
    fn system4_amplifies_strategies() {
        let bb = temp_board();
        let ctrl = VsmController::new(bb);
        let strategies = vec!["s1".to_string()];
        let expanded = ctrl.system4().amplify_strategies(strategies, 3);
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded, vec!["s1", "s1", "s1"]);
    }

    #[test]
    fn variety_engine_is_available_on_controller() {
        let bb = temp_board();
        let ctrl = VsmController::new(bb);
        let reduced = VarietyEngine::attenuate_variety(vec!["x".to_string()], 5);
        assert_eq!(reduced.len(), 1);
        // Verifica que o controller possui a engine
        let _ = ctrl.variety_engine();
    }

    #[test]
    fn operational_status_report() {
        let s1 = System1Operations;
        let results = vec![
            TaskResult {
                task_id: "t1".to_string(),
                success: true,
                output: Value::Null,
                duration_ms: 100,
            },
            TaskResult {
                task_id: "t2".to_string(),
                success: true,
                output: Value::Null,
                duration_ms: 200,
            },
        ];
        let status = s1.get_operational_status(&results);
        assert_eq!(status.tasks_executed, 2);
        assert_eq!(status.tasks_failed, 0);
        assert_eq!(status.avg_duration_ms, 150);
        assert_eq!(status.last_task_id, Some("t2".to_string()));
    }

    #[test]
    fn resource_allocation_basic() {
        let s2 = System2Coordination;
        let requests = vec![ResourceRequest {
            task_id: "only".to_string(),
            resource_type: "mem".to_string(),
            amount: 50,
            priority: 1,
        }];
        let allocs = s2.resolve_resource_conflict(&requests, 100);
        assert_eq!(allocs.len(), 1);
        assert_eq!(allocs[0].granted, 50);
        assert_eq!(allocs[0].reason, "full");
    }

    #[test]
    fn performance_report_generation() {
        let s3 = System3Control;
        let status = OperationalStatus {
            tasks_executed: 10,
            tasks_failed: 2,
            avg_duration_ms: 100,
            last_task_id: None,
        };
        let report = s3.generate_performance_report(&status);
        assert!((report.error_rate - 0.2).abs() < 0.001);
        assert!(report.throughput > 0.0);
        assert!(report.utilization > 0.0);
    }

    #[test]
    fn policy_authorization() {
        let s5 = System5Policy::new();
        let low = DeviationRequest {
            reason: "teste".to_string(),
            requested_action: "skip".to_string(),
            risk_level: 0.1,
        };
        let auth_low = s5.authorize_deviation(&low);
        assert!(auth_low.approved);

        let high = DeviationRequest {
            reason: "teste".to_string(),
            requested_action: "rm -rf".to_string(),
            risk_level: 0.9,
        };
        let auth_high = s5.authorize_deviation(&high);
        assert!(!auth_high.approved);
    }

    #[test]
    fn vsm_persist_and_load_status() {
        let bb = temp_board();
        let ctrl = VsmController::new(bb);
        let status = OperationalStatus {
            tasks_executed: 5,
            tasks_failed: 1,
            avg_duration_ms: 42,
            last_task_id: Some("x".to_string()),
        };
        ctrl.persist_status(&status).unwrap();
        let loaded = ctrl.load_status().unwrap();
        assert_eq!(loaded, status);
    }

    #[test]
    fn schedule_operations_respects_deps() {
        let s2 = System2Coordination;
        let ops = vec![
            Operation {
                id: "c".to_string(),
                depends_on: vec!["b".to_string()],
                estimated_duration_ms: 10,
            },
            Operation {
                id: "a".to_string(),
                depends_on: vec![],
                estimated_duration_ms: 5,
            },
            Operation {
                id: "b".to_string(),
                depends_on: vec!["a".to_string()],
                estimated_duration_ms: 7,
            },
        ];
        let sched = s2.schedule_operations(&ops);
        assert_eq!(sched.order, vec!["a", "b", "c"]);
        assert_eq!(sched.estimated_total_ms, 22);
    }

    #[test]
    fn system3_enforce_constraints_ok() {
        let s3 = System3Control;
        let constraints = vec![Constraint {
            name: "max_tasks".to_string(),
            max_value: 10,
            current_value: 8,
        }];
        assert!(s3.enforce_constraints(&constraints).is_ok());
    }

    #[test]
    fn system3_enforce_constraints_fails() {
        let s3 = System3Control;
        let constraints = vec![Constraint {
            name: "max_tasks".to_string(),
            max_value: 5,
            current_value: 6,
        }];
        assert!(s3.enforce_constraints(&constraints).is_err());
    }
}
