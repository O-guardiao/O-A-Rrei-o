pub mod goal;
pub mod hitl;
pub mod ooda_c;
pub mod problem_space;

pub use hitl::{ComplianceChecker, ComplianceResult, HumanInputPoller, PollResult};
pub use ooda_c::{
    CycleResult, EssentialVariables, IGCResult, OODACLoop, OODAConfig, OodaPhase,
    UltraStableTrigger,
};
pub use problem_space::{
    Impasse, ImpasseClassifier, ImpasseType, Operator, ProblemSpace, ProblemSpaceEngine, State,
    WeakMethod,
};

use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Iteration Budget ──────────────────────────────────────────────────────────

/// Contador de iterações com grace call (padrão Hermes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IterationBudget {
    pub max: u32,
    pub remaining: u32,
    pub grace_used: bool,
}

impl IterationBudget {
    /// Budget padrão: 90 iterações + 1 grace call.
    pub fn new(max: u32) -> Self {
        Self {
            max,
            remaining: max,
            grace_used: false,
        }
    }

    pub fn default_budget() -> Self {
        Self::new(90)
    }

    /// Consome uma iteração. Retorna false se esgotado (sem grace).
    pub fn consume(&mut self) -> bool {
        if self.remaining > 0 {
            self.remaining -= 1;
            true
        } else {
            false
        }
    }

    /// Retorna true se ainda há iterações ou grace call disponível.
    pub fn can_proceed(&self) -> bool {
        self.remaining > 0 || !self.grace_used
    }

    /// Consome o grace call. Retorna true se foi consumido agora.
    pub fn use_grace(&mut self) -> bool {
        if self.remaining == 0 && !self.grace_used {
            self.grace_used = true;
            true
        } else {
            false
        }
    }

    /// Retorna true se o budget está esgotado (incluindo grace).
    pub fn is_exhausted(&self) -> bool {
        self.remaining == 0 && self.grace_used
    }
}

// ── Modo FSM ──────────────────────────────────────────────────────────────────

/// Modo de operação da FSM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsmMode {
    Task,
    Conversational,
}

impl fmt::Display for FsmMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FsmMode::Task => write!(f, "task"),
            FsmMode::Conversational => write!(f, "conversational"),
        }
    }
}

impl FsmMode {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "task" => Some(Self::Task),
            "conversational" => Some(Self::Conversational),
            _ => None,
        }
    }

    /// Budget padrão para este modo.
    pub fn default_budget(&self) -> IterationBudget {
        match self {
            FsmMode::Task => IterationBudget::new(20),
            FsmMode::Conversational => IterationBudget::new(90),
        }
    }
}

// ── Estados ───────────────────────────────────────────────────────────────────

/// Ciclo de vida de uma sessão agentiva.
/// A IA é invocada *stateless* — recebe apenas o pacote do estado atual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Idle,
    Exploration,
    Planning,
    Execution,
    Evaluation,
    Correction,
    Consolidation,
    /// Watchdog disparou (3× mesmo erro). Agente reavalia do zero.
    StrategicRetreat,
    /// Verifica se ação precisa de aprovação humana (PVC-Q1.2).
    ComplianceCheck,
    /// Aguardando decisão humana no Blackboard (PVC-Q1.2).
    AwaitingHumanInput,
    /// Aprovação humana recebida — prossegue execução (PVC-Q1.2).
    HumanApproved,
    /// Rejeição humana recebida — aborta ou replaneja (PVC-Q1.2).
    HumanRejected,
    /// Timeout ou sem aprovador disponível — escalado para auditor (PVC-Q1.2).
    EscalatedToAuditor,
    /// ReAct harnessed: LLM produz um Thought auditável (PVC-Q2.1).
    ReasoningThought,
    /// ReAct harnessed: harness executa a ação proposta (PVC-Q2.1).
    ReasoningAction,
    /// ReAct harnessed: observação do resultado injetada no próximo ciclo (PVC-Q2.1).
    ReasoningObservation,
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl AgentState {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Idle" => Some(Self::Idle),
            "Exploration" => Some(Self::Exploration),
            "Planning" => Some(Self::Planning),
            "Execution" => Some(Self::Execution),
            "Evaluation" => Some(Self::Evaluation),
            "Correction" => Some(Self::Correction),
            "Consolidation" => Some(Self::Consolidation),
            "StrategicRetreat" => Some(Self::StrategicRetreat),
            "ComplianceCheck" => Some(Self::ComplianceCheck),
            "AwaitingHumanInput" => Some(Self::AwaitingHumanInput),
            "HumanApproved" => Some(Self::HumanApproved),
            "HumanRejected" => Some(Self::HumanRejected),
            "EscalatedToAuditor" => Some(Self::EscalatedToAuditor),
            "ReasoningThought" => Some(Self::ReasoningThought),
            "ReasoningAction" => Some(Self::ReasoningAction),
            "ReasoningObservation" => Some(Self::ReasoningObservation),
            _ => None,
        }
    }
}

// ── Continue Sites e Recovery Cascade ─────────────────────────────────────────

/// Motivo de transição que dispara um continue site no loop principal.
/// Inspirado nos 7 continue sites do Claude Code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionReason {
    /// Context window cheia — tentar compactar e retry.
    CollapseDrainRetry,
    /// Compactação reativa disparada — retry com contexto reduzido.
    ReactiveCompactRetry,
    /// Modelo atingiu max_output_tokens — escalar para modelo maior.
    MaxOutputTokensEscalate,
    /// Modelo maior também falhou — recovery com modelo diferente.
    MaxOutputTokensRecovery,
    /// Hook de stop bloqueou a execução — perguntar ao usuário.
    StopHookBlocking,
    /// Budget de tokens esgotado — continuação com orçamento renovado.
    TokenBudgetContinuation,
    /// Próximo turno normal.
    NextTurn,
}

impl fmt::Display for TransitionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Ação de recovery ordenada por custo (do mais barato ao mais caro).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryAction {
    /// Retry simples com o mesmo contexto (custo: zero tokens).
    Retry,
    /// Compactar contexto e retry (custo: ~n tokens de overhead).
    Compact,
    /// Escalar para modelo com maior context window (custo: modelo mais caro).
    EscalateModel,
    /// Fallback para modelo alternativo (custo: modelo diferente).
    FallbackModel,
    /// Perguntar ao usuário como proceder (custo: humano).
    AskUser,
    /// Abortar a execução (custo: sessão perdida).
    Abort,
}

impl RecoveryAction {
    /// Custo relativo da ação (menor = mais barato).
    pub fn cost(&self) -> u8 {
        match self {
            RecoveryAction::Retry => 0,
            RecoveryAction::Compact => 1,
            RecoveryAction::EscalateModel => 2,
            RecoveryAction::FallbackModel => 3,
            RecoveryAction::AskUser => 4,
            RecoveryAction::Abort => 5,
        }
    }
}

/// Rastreador de ações de recovery já tentadas para evitar repetição.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryTracker {
    pub attempts: Vec<RecoveryAction>,
}

impl RecoveryTracker {
    pub fn new() -> Self {
        Self {
            attempts: Vec::new(),
        }
    }

    /// Registra uma tentativa de recovery.
    pub fn record(&mut self, action: &RecoveryAction) {
        self.attempts.push(action.clone());
    }

    /// Retorna true se a ação já foi tentada.
    pub fn was_attempted(&self, action: &RecoveryAction) -> bool {
        self.attempts.contains(action)
    }

    /// Retorna a próxima ação não tentada, ordenada por custo.
    pub fn next_unattempted(&self, candidates: &[RecoveryAction]) -> Option<RecoveryAction> {
        candidates
            .iter()
            .filter(|a| !self.was_attempted(a))
            .min_by_key(|a| a.cost())
            .cloned()
    }
}

/// Decide a estratégia de recovery mais barata não esgotada.
///
/// # Exemplo
/// ```
/// use arreio_fsm::{TransitionReason, RecoveryAction, RecoveryTracker, decide_recovery_strategy};
///
/// let mut tracker = RecoveryTracker::new();
/// let action = decide_recovery_strategy(&TransitionReason::CollapseDrainRetry, &tracker);
/// assert_eq!(action, RecoveryAction::Compact);
/// ```
pub fn decide_recovery_strategy(
    reason: &TransitionReason,
    tracker: &RecoveryTracker,
) -> RecoveryAction {
    let candidates: Vec<RecoveryAction> = match reason {
        TransitionReason::CollapseDrainRetry => {
            vec![
                RecoveryAction::Compact,
                RecoveryAction::EscalateModel,
                RecoveryAction::AskUser,
            ]
        }
        TransitionReason::ReactiveCompactRetry => {
            vec![
                RecoveryAction::Retry,
                RecoveryAction::Compact,
                RecoveryAction::FallbackModel,
                RecoveryAction::AskUser,
            ]
        }
        TransitionReason::MaxOutputTokensEscalate => {
            vec![
                RecoveryAction::EscalateModel,
                RecoveryAction::FallbackModel,
                RecoveryAction::AskUser,
            ]
        }
        TransitionReason::MaxOutputTokensRecovery => {
            vec![
                RecoveryAction::FallbackModel,
                RecoveryAction::AskUser,
                RecoveryAction::Abort,
            ]
        }
        TransitionReason::StopHookBlocking => {
            vec![RecoveryAction::AskUser]
        }
        TransitionReason::TokenBudgetContinuation => {
            vec![
                RecoveryAction::Retry,
                RecoveryAction::Compact,
                RecoveryAction::AskUser,
            ]
        }
        TransitionReason::NextTurn => {
            vec![RecoveryAction::Retry]
        }
    };

    tracker
        .next_unattempted(&candidates)
        .unwrap_or(RecoveryAction::Abort)
}

// ── Transições válidas ────────────────────────────────────────────────────────

fn is_valid(from: &AgentState, to: &AgentState) -> bool {
    use AgentState::*;
    matches!(
        (from, to),
        (Idle,             Exploration)
        | (Exploration,    Planning)
        | (Planning,       Execution)
        | (Execution,      Evaluation)
        | (Execution,      ComplianceCheck)   // PVC-Q1.2: verifica compliance
        | (Evaluation,     Consolidation)     // sucesso
        | (Evaluation,     Correction)        // falha
        | (Evaluation,     StrategicRetreat)  // abort após avaliação
        | (Correction,     Execution)         // retry
        | (Correction,     StrategicRetreat)  // watchdog
        | (StrategicRetreat, Planning)        // replanejamento
        | (StrategicRetreat, Idle)            // reset
        // PVC-Q1.2: transições HITL
        | (ComplianceCheck, AwaitingHumanInput)  // requer aprovação
        | (ComplianceCheck, Execution)           // auto-aprovação
        | (ComplianceCheck, StrategicRetreat)    // auto-rejeição
        | (AwaitingHumanInput, HumanApproved)    // decisão recebida
        | (AwaitingHumanInput, HumanRejected)    // decisão recebida
        | (AwaitingHumanInput, EscalatedToAuditor) // timeout
        | (HumanApproved, Execution)             // prossegue
        | (HumanRejected, StrategicRetreat)      // aborta
        | (EscalatedToAuditor, StrategicRetreat) // aborta após escalation
        // PVC-Q2.1: ciclo ReAct harnessed (Thought → Action → Observation)
        | (Planning,  ReasoningThought)          // reasoning antes da execução
        | (Execution, ReasoningThought)          // reasoning durante execução
        | (ReasoningThought, ReasoningAction)    // ação proposta pelo LLM
        | (ReasoningThought, Evaluation)         // FINAL sem ação
        | (ReasoningThought, StrategicRetreat)   // budget esgotado
        | (ReasoningAction, ReasoningObservation) // harness executou a ação
        | (ReasoningAction, StrategicRetreat)    // ação negada/budget esgotado
        | (ReasoningObservation, ReasoningThought) // próximo ciclo
        | (ReasoningObservation, Evaluation)     // ciclo concluiu a tarefa
        | (ReasoningObservation, StrategicRetreat) // budget esgotado
        | (_, Idle) // reset de qualquer estado
    )
}

// ── FSM ───────────────────────────────────────────────────────────────────────

/// Máquina de estado persistida no Blackboard.
/// O estado atual vive em `category="fsm", key="current"`.
pub struct Fsm {
    blackboard: Blackboard,
}

impl Fsm {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn current(&self) -> AgentState {
        self.blackboard
            .get_tuple("fsm", "current")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .and_then(|s| AgentState::from_str(&s))
            .unwrap_or(AgentState::Idle)
    }

    /// Lê o budget de iterações do Blackboard. Cria default se não existir.
    pub fn budget(&self) -> IterationBudget {
        self.blackboard
            .get_tuple("fsm", "budget")
            .and_then(|v| serde_json::from_value::<IterationBudget>(v).ok())
            .unwrap_or_else(IterationBudget::default_budget)
    }

    /// Persiste o budget no Blackboard.
    pub fn set_budget(&self, budget: &IterationBudget) -> Result<()> {
        self.blackboard
            .put_tuple("fsm", "budget", serde_json::to_value(budget)?)
    }

    /// Consome uma iteração do budget. Retorna erro se esgotado.
    pub fn consume_iteration(&self) -> Result<()> {
        let mut budget = self.budget();
        if budget.is_exhausted() {
            bail!("budget esgotado: {} iterações consumidas", budget.max);
        }
        if budget.remaining > 0 {
            budget.consume();
            self.set_budget(&budget)?;
        } else if !budget.use_grace() {
            bail!("budget esgotado (grace call já usada)");
        } else {
            self.set_budget(&budget)?;
        }
        Ok(())
    }

    /// Transita para o próximo estado, validando a transição e consumindo budget.
    pub fn transition(&self, to: AgentState) -> Result<()> {
        let from = self.current();
        if !is_valid(&from, &to) {
            bail!("transição inválida: {} → {}", from, to);
        }
        // Não consome budget para Idle (reset) nem StrategicRetreat (watchdog).
        if to != AgentState::Idle && to != AgentState::StrategicRetreat {
            self.consume_iteration()?;
        }
        self.blackboard
            .put_tuple("fsm", "current", serde_json::json!(to.to_string()))?;
        Ok(())
    }

    /// Força transição para StrategicRetreat (chamado pelo Watchdog).
    pub fn interrupt(&self) -> Result<()> {
        self.blackboard
            .put_tuple("fsm", "current", serde_json::json!("StrategicRetreat"))?;
        Ok(())
    }

    /// Reseta o budget para o default.
    pub fn reset_budget(&self) -> Result<()> {
        self.set_budget(&IterationBudget::default_budget())
    }

    /// Lê o modo atual da FSM.
    pub fn mode(&self) -> FsmMode {
        self.blackboard
            .get_tuple("fsm", "mode")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .and_then(|s| FsmMode::from_str(&s))
            .unwrap_or(FsmMode::Task)
    }

    /// Define o modo da FSM e reseta o budget para o padrão do modo.
    pub fn set_mode(&self, mode: FsmMode) -> Result<()> {
        self.blackboard
            .put_tuple("fsm", "mode", serde_json::json!(mode.to_string()))?;
        self.set_budget(&mode.default_budget())
    }

    /// Reseta o budget para o padrão do modo atual.
    pub fn reset_budget_for_mode(&self) -> Result<()> {
        self.set_budget(&self.mode().default_budget())
    }

    // ── Continue Sites / Recovery ──────────────────────────────────────────────

    /// Lê o motivo da última transição (continue site).
    pub fn transition_reason(&self) -> Option<TransitionReason> {
        self.blackboard
            .get_tuple("fsm", "transition_reason")
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Define o motivo da transição atual.
    pub fn set_transition_reason(&self, reason: &TransitionReason) -> Result<()> {
        self.blackboard
            .put_tuple("fsm", "transition_reason", serde_json::to_value(reason)?)
    }

    /// Limpa o motivo da transição.
    pub fn clear_transition_reason(&self) -> Result<()> {
        self.blackboard
            .put_tuple("fsm", "transition_reason", serde_json::json!(null))
    }

    /// Lê o recovery tracker (histórico de tentativas de recovery).
    pub fn recovery_tracker(&self) -> RecoveryTracker {
        self.blackboard
            .get_tuple("fsm", "recovery_tracker")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    /// Persiste o recovery tracker.
    pub fn set_recovery_tracker(&self, tracker: &RecoveryTracker) -> Result<()> {
        self.blackboard
            .put_tuple("fsm", "recovery_tracker", serde_json::to_value(tracker)?)
    }

    /// Reseta o recovery tracker (usado após sucesso ou novo turno).
    pub fn reset_recovery_tracker(&self) -> Result<()> {
        self.set_recovery_tracker(&RecoveryTracker::new())
    }

    /// Transita para o próximo estado registrando o motivo da transição.
    /// Integra recovery cascade: decide ação de recovery baseada no motivo.
    pub fn transition_with_reason(
        &self,
        to: AgentState,
        reason: &TransitionReason,
    ) -> Result<RecoveryAction> {
        let mut tracker = self.recovery_tracker();
        let action = decide_recovery_strategy(reason, &tracker);
        tracker.record(&action);
        self.set_recovery_tracker(&tracker)?;
        self.set_transition_reason(reason)?;
        self.transition(to)?;
        Ok(action)
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_fsm() -> Fsm {
        let f = NamedTempFile::new().unwrap();
        let path: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = arreio_kernel::Blackboard::open(&path).unwrap();
        Fsm::new(bb)
    }

    #[test]
    fn initial_state_is_idle() {
        let fsm = temp_fsm();
        assert_eq!(fsm.current(), AgentState::Idle);
    }

    #[test]
    fn valid_chain_idle_to_consolidation() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::Execution).unwrap();
        fsm.transition(AgentState::Evaluation).unwrap();
        fsm.transition(AgentState::Consolidation).unwrap();
        assert_eq!(fsm.current(), AgentState::Consolidation);
    }

    #[test]
    fn invalid_transition_returns_error() {
        let fsm = temp_fsm();
        assert!(fsm.transition(AgentState::Execution).is_err()); // Idle → Execution inválido
    }

    #[test]
    fn correction_loop_then_retreat() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::Execution).unwrap();
        fsm.transition(AgentState::Evaluation).unwrap();
        fsm.transition(AgentState::Correction).unwrap();
        fsm.transition(AgentState::StrategicRetreat).unwrap();
        assert_eq!(fsm.current(), AgentState::StrategicRetreat);
    }

    #[test]
    fn interrupt_forces_strategic_retreat_from_any_state() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.interrupt().unwrap();
        assert_eq!(fsm.current(), AgentState::StrategicRetreat);
    }

    #[test]
    fn reset_to_idle_from_any_state() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::Idle).unwrap();
        assert_eq!(fsm.current(), AgentState::Idle);
    }

    #[test]
    fn budget_default_is_90_plus_grace() {
        let fsm = temp_fsm();
        let budget = fsm.budget();
        assert_eq!(budget.max, 90);
        assert_eq!(budget.remaining, 90);
        assert!(!budget.grace_used);
    }

    #[test]
    fn consume_iteration_reduces_budget() {
        let fsm = temp_fsm();
        fsm.consume_iteration().unwrap();
        let budget = fsm.budget();
        assert_eq!(budget.remaining, 89);
    }

    #[test]
    fn grace_call_allowed_after_budget_zero() {
        let fsm = temp_fsm();
        fsm.set_budget(&IterationBudget {
            max: 1,
            remaining: 0,
            grace_used: false,
        })
        .unwrap();
        // Primeira chamada após esgotamento usa grace.
        fsm.consume_iteration().unwrap();
        let budget = fsm.budget();
        assert!(budget.grace_used);
    }

    #[test]
    fn budget_exhausted_after_grace() {
        let fsm = temp_fsm();
        fsm.set_budget(&IterationBudget {
            max: 1,
            remaining: 0,
            grace_used: true,
        })
        .unwrap();
        assert!(fsm.consume_iteration().is_err());
    }

    #[test]
    fn transition_to_idle_does_not_consume_budget() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        let budget_after_exploration = fsm.budget().remaining;
        fsm.transition(AgentState::Idle).unwrap();
        let budget_after_idle = fsm.budget().remaining;
        assert_eq!(budget_after_exploration, budget_after_idle);
    }

    #[test]
    fn modo_conversational_budget_90() {
        let fsm = temp_fsm();
        assert_eq!(fsm.mode(), FsmMode::Task);
        fsm.set_mode(FsmMode::Conversational).unwrap();
        assert_eq!(fsm.mode(), FsmMode::Conversational);
        let budget = fsm.budget();
        assert_eq!(budget.max, 90);
        assert_eq!(budget.remaining, 90);
    }

    #[test]
    fn modo_task_budget_20() {
        let fsm = temp_fsm();
        fsm.set_mode(FsmMode::Task).unwrap();
        let budget = fsm.budget();
        assert_eq!(budget.max, 20);
        assert_eq!(budget.remaining, 20);
    }

    #[test]
    fn reset_budget_for_mode_respeita_modo() {
        let fsm = temp_fsm();
        fsm.set_mode(FsmMode::Conversational).unwrap();
        fsm.consume_iteration().unwrap();
        fsm.consume_iteration().unwrap();
        assert_eq!(fsm.budget().remaining, 88);

        fsm.reset_budget_for_mode().unwrap();
        assert_eq!(fsm.budget().remaining, 90);
    }

    // ── Continue Sites / Recovery ─────────────────────────────────────────────

    #[test]
    fn transition_reason_persisted() {
        let fsm = temp_fsm();
        assert!(fsm.transition_reason().is_none());

        fsm.set_transition_reason(&TransitionReason::CollapseDrainRetry)
            .unwrap();
        assert_eq!(
            fsm.transition_reason(),
            Some(TransitionReason::CollapseDrainRetry)
        );

        fsm.clear_transition_reason().unwrap();
        assert!(fsm.transition_reason().is_none());
    }

    #[test]
    fn recovery_tracker_records_attempts() {
        let fsm = temp_fsm();
        let mut tracker = fsm.recovery_tracker();
        assert!(tracker.attempts.is_empty());

        tracker.record(&RecoveryAction::Retry);
        tracker.record(&RecoveryAction::Compact);
        fsm.set_recovery_tracker(&tracker).unwrap();

        let loaded = fsm.recovery_tracker();
        assert_eq!(loaded.attempts.len(), 2);
        assert!(loaded.was_attempted(&RecoveryAction::Retry));
        assert!(loaded.was_attempted(&RecoveryAction::Compact));
        assert!(!loaded.was_attempted(&RecoveryAction::Abort));
    }

    #[test]
    fn decide_recovery_collapse_drain_prefers_compact() {
        let tracker = RecoveryTracker::new();
        let action = decide_recovery_strategy(&TransitionReason::CollapseDrainRetry, &tracker);
        assert_eq!(action, RecoveryAction::Compact);
    }

    #[test]
    fn decide_recovery_skips_already_attempted() {
        let mut tracker = RecoveryTracker::new();
        tracker.record(&RecoveryAction::Compact);
        let action = decide_recovery_strategy(&TransitionReason::CollapseDrainRetry, &tracker);
        // Compact já foi tentado, próximo mais barato é EscalateModel
        assert_eq!(action, RecoveryAction::EscalateModel);
    }

    #[test]
    fn decide_recovery_all_exhausted_aborts() {
        let mut tracker = RecoveryTracker::new();
        tracker.record(&RecoveryAction::Compact);
        tracker.record(&RecoveryAction::EscalateModel);
        tracker.record(&RecoveryAction::AskUser);
        let action = decide_recovery_strategy(&TransitionReason::CollapseDrainRetry, &tracker);
        assert_eq!(action, RecoveryAction::Abort);
    }

    #[test]
    fn decide_recovery_stop_hook_asks_user() {
        let tracker = RecoveryTracker::new();
        let action = decide_recovery_strategy(&TransitionReason::StopHookBlocking, &tracker);
        assert_eq!(action, RecoveryAction::AskUser);
    }

    #[test]
    fn transition_with_reason_integrates_recovery() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::Execution).unwrap();

        let action = fsm
            .transition_with_reason(
                AgentState::Evaluation,
                &TransitionReason::CollapseDrainRetry,
            )
            .unwrap();

        assert_eq!(action, RecoveryAction::Compact);
        assert_eq!(fsm.current(), AgentState::Evaluation);
        assert_eq!(
            fsm.transition_reason(),
            Some(TransitionReason::CollapseDrainRetry)
        );

        let tracker = fsm.recovery_tracker();
        assert_eq!(tracker.attempts, vec![RecoveryAction::Compact]);
    }

    #[test]
    fn recovery_action_cost_ordering() {
        assert_eq!(RecoveryAction::Retry.cost(), 0);
        assert_eq!(RecoveryAction::Compact.cost(), 1);
        assert_eq!(RecoveryAction::EscalateModel.cost(), 2);
        assert_eq!(RecoveryAction::FallbackModel.cost(), 3);
        assert_eq!(RecoveryAction::AskUser.cost(), 4);
        assert_eq!(RecoveryAction::Abort.cost(), 5);
    }

    // ── PVC-Q2.1: estados ReAct harnessed ─────────────────────────────────────

    #[test]
    fn ciclo_react_completo_planning_ate_evaluation() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::ReasoningThought).unwrap();
        fsm.transition(AgentState::ReasoningAction).unwrap();
        fsm.transition(AgentState::ReasoningObservation).unwrap();
        // segundo ciclo
        fsm.transition(AgentState::ReasoningThought).unwrap();
        // FINAL sem ação → Evaluation
        fsm.transition(AgentState::Evaluation).unwrap();
        assert_eq!(fsm.current(), AgentState::Evaluation);
    }

    #[test]
    fn react_a_partir_de_execution() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::Execution).unwrap();
        fsm.transition(AgentState::ReasoningThought).unwrap();
        assert_eq!(fsm.current(), AgentState::ReasoningThought);
    }

    #[test]
    fn react_budget_esgotado_vai_para_strategic_retreat() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::ReasoningThought).unwrap();
        fsm.transition(AgentState::StrategicRetreat).unwrap();
        assert_eq!(fsm.current(), AgentState::StrategicRetreat);
    }

    #[test]
    fn react_observation_nao_pula_para_action() {
        let fsm = temp_fsm();
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();
        fsm.transition(AgentState::ReasoningThought).unwrap();
        fsm.transition(AgentState::ReasoningAction).unwrap();
        fsm.transition(AgentState::ReasoningObservation).unwrap();
        // Observation → Action direto é inválido (precisa passar por Thought)
        assert!(fsm.transition(AgentState::ReasoningAction).is_err());
    }

    #[test]
    fn estados_react_roundtrip_from_str() {
        for s in [
            "ReasoningThought",
            "ReasoningAction",
            "ReasoningObservation",
        ] {
            let state = AgentState::from_str(s).unwrap();
            assert_eq!(state.to_string(), s);
        }
    }
}
