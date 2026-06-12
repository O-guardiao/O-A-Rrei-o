use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Fases OODA-C ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OodaPhase {
    Observe,
    Orient,
    Decide,
    Act,
}

impl fmt::Display for OodaPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl OodaPhase {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Observe" => Some(Self::Observe),
            "Orient" => Some(Self::Orient),
            "Decide" => Some(Self::Decide),
            "Act" => Some(Self::Act),
            _ => None,
        }
    }
}

// ── Configuração ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OODAConfig {
    pub theta_igc: f64,
    pub theta_decide: f64,
    pub max_orient_ms: u64,
    pub max_decide_ms: u64,
}

impl Default for OODAConfig {
    fn default() -> Self {
        Self {
            theta_igc: 0.85,
            theta_decide: 0.60,
            max_orient_ms: 500,
            max_decide_ms: 100,
        }
    }
}

// ── Variáveis Essenciais (Ashby) ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EssentialVariables {
    pub eir: f64,
    pub confidence: f64,
    pub token_budget_used: f64,
    pub latency_ms: u64,
}

impl Default for EssentialVariables {
    fn default() -> Self {
        Self {
            eir: 0.0,
            confidence: 1.0,
            token_budget_used: 0.0,
            latency_ms: 0,
        }
    }
}

// ── Resultado IG&C ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IGCResult {
    BypassToAct { reason: String },
    FullDeliberation { reason: String },
}

// ── Trigger de Ultrastabilidade ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UltraStableTrigger {
    TokenBudgetExhausted,
    ErrorRateTooHigh,
    LatencySLAViolation,
    ConfidenceTooLow,
}

// ── Resultado do Ciclo ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleResult {
    pub phase_transitions: Vec<OodaPhase>,
    pub action: String,
    pub used_igc_bypass: bool,
    pub essential_vars: EssentialVariables,
}

// ── Transições válidas ────────────────────────────────────────────────────────

fn is_valid_phase_transition(from: &OodaPhase, to: &OodaPhase) -> bool {
    use OodaPhase::*;
    matches!(
        (from, to),
        (Observe, Orient) | (Orient, Decide) | (Orient, Act) | (Decide, Act) | (Act, Observe)
    )
}

// ── OODA-C Loop ───────────────────────────────────────────────────────────────

pub struct OODACLoop {
    blackboard: Blackboard,
    session_id: String,
    config: OODAConfig,
}

impl OODACLoop {
    pub fn new(blackboard: Blackboard, session_id: &str) -> Self {
        let loop_ = Self {
            blackboard,
            session_id: session_id.to_string(),
            config: OODAConfig::default(),
        };
        // Inicializa fase para Observe caso não exista
        if loop_
            .blackboard
            .get_tuple("ooda", &loop_.bb_key("phase"))
            .is_none()
        {
            let _ = loop_.blackboard.put_tuple(
                "ooda",
                &loop_.bb_key("phase"),
                serde_json::json!(OodaPhase::Observe.to_string()),
            );
        }
        loop_
    }

    pub fn with_config(mut self, config: OODAConfig) -> Self {
        self.config = config;
        self
    }

    fn bb_key(&self, suffix: &str) -> String {
        format!("{}:{}", self.session_id, suffix)
    }

    /// Persiste a configuração no Blackboard.
    pub fn save_config(&self) -> Result<()> {
        let value = serde_json::to_value(&self.config)?;
        self.blackboard
            .put_tuple("ooda", &self.bb_key("config"), value)?;
        Ok(())
    }

    /// Carrega a configuração do Blackboard.
    pub fn load_config(&self) -> Option<OODAConfig> {
        self.blackboard
            .get_tuple("ooda", &self.bb_key("config"))
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Executa um ciclo completo OODA-C.
    pub fn run_cycle(&self, observation: &str, confidence: f64) -> Result<CycleResult> {
        let mut phase_transitions = vec![];

        // Observe (idempotente: permite reentrada no início do ciclo)
        if self.current_phase() != OodaPhase::Observe {
            self.transition_phase(OodaPhase::Observe)?;
        }
        phase_transitions.push(OodaPhase::Observe);

        // Orient
        self.transition_phase(OodaPhase::Orient)?;
        phase_transitions.push(OodaPhase::Orient);

        let igc = self.check_igc(confidence);
        let used_igc_bypass = matches!(igc, IGCResult::BypassToAct { .. });

        match igc {
            IGCResult::BypassToAct { .. } => {
                self.transition_phase(OodaPhase::Act)?;
                phase_transitions.push(OodaPhase::Act);
            }
            IGCResult::FullDeliberation { .. } => {
                self.transition_phase(OodaPhase::Decide)?;
                phase_transitions.push(OodaPhase::Decide);

                self.transition_phase(OodaPhase::Act)?;
                phase_transitions.push(OodaPhase::Act);
            }
        }

        let essential_vars = self.get_essential_vars().unwrap_or_default();

        Ok(CycleResult {
            phase_transitions,
            action: observation.to_string(),
            used_igc_bypass,
            essential_vars,
        })
    }

    /// Verifica IG&C: podemos fazer bypass de Decide direto para Act?
    pub fn check_igc(&self, confidence: f64) -> IGCResult {
        if confidence >= self.config.theta_igc {
            IGCResult::BypassToAct {
                reason: format!(
                    "confiança {} >= theta_igc {}",
                    confidence, self.config.theta_igc
                ),
            }
        } else {
            IGCResult::FullDeliberation {
                reason: format!(
                    "confiança {} < theta_igc {}",
                    confidence, self.config.theta_igc
                ),
            }
        }
    }

    /// Atualiza variáveis essenciais após um turno.
    pub fn update_essential_vars(&self, vars: &EssentialVariables) -> Result<()> {
        let value = serde_json::to_value(vars)?;
        self.blackboard
            .put_tuple("ooda", &self.bb_key("essential_vars"), value)?;
        Ok(())
    }

    /// Recupera variáveis essenciais.
    pub fn get_essential_vars(&self) -> Option<EssentialVariables> {
        self.blackboard
            .get_tuple("ooda", &self.bb_key("essential_vars"))
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Verifica se alguma variável essencial está fora dos limites (trigger de ultrastabilidade).
    pub fn check_ultrastability(&self) -> Option<UltraStableTrigger> {
        let vars = self.get_essential_vars().unwrap_or_default();

        if vars.token_budget_used >= 1.0 {
            return Some(UltraStableTrigger::TokenBudgetExhausted);
        }
        if vars.eir >= 0.5 {
            return Some(UltraStableTrigger::ErrorRateTooHigh);
        }
        if vars.latency_ms > 5000 {
            return Some(UltraStableTrigger::LatencySLAViolation);
        }
        if vars.confidence < 0.3 {
            return Some(UltraStableTrigger::ConfidenceTooLow);
        }

        None
    }

    /// Retorna a fase atual.
    pub fn current_phase(&self) -> OodaPhase {
        self.blackboard
            .get_tuple("ooda", &self.bb_key("phase"))
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .and_then(|s| OodaPhase::from_str(&s))
            .unwrap_or(OodaPhase::Observe)
    }

    /// Transita para a próxima fase, validando a transição.
    pub fn transition_phase(&self, phase: OodaPhase) -> Result<()> {
        let from = self.current_phase();
        if !is_valid_phase_transition(&from, &phase) {
            bail!("transição de fase inválida: {} → {}", from, phase);
        }
        self.blackboard.put_tuple(
            "ooda",
            &self.bb_key("phase"),
            serde_json::json!(phase.to_string()),
        )?;
        Ok(())
    }

    // ── Integração com FSM ─────────────────────────────────────────────────────

    /// Mapeia um estado da FSM para uma fase OODA-C.
    pub fn map_fsm_state(fsm_state: &crate::AgentState) -> OodaPhase {
        use crate::AgentState::*;
        match fsm_state {
            Idle | Exploration | ReasoningObservation => OodaPhase::Observe,
            Planning | Correction | ReasoningThought => OodaPhase::Orient,
            Evaluation | ComplianceCheck | AwaitingHumanInput => OodaPhase::Decide,
            Execution | Consolidation | StrategicRetreat | HumanApproved | HumanRejected
            | EscalatedToAuditor | ReasoningAction => OodaPhase::Act,
        }
    }

    /// Sincroniza a fase OODA-C com o estado atual da FSM.
    pub fn sync_with_fsm(&self, fsm: &crate::Fsm) -> Result<()> {
        let state = fsm.current();
        let phase = Self::map_fsm_state(&state);
        self.transition_phase(phase)
    }
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

    fn temp_ooda() -> OODACLoop {
        OODACLoop::new(temp_bb(), "test-session")
    }

    #[test]
    fn initial_phase_is_observe() {
        let ooda = temp_ooda();
        assert_eq!(ooda.current_phase(), OodaPhase::Observe);
    }

    #[test]
    fn igc_bypass_when_high_confidence() {
        let ooda = temp_ooda();
        let result = ooda.check_igc(0.90);
        assert!(matches!(result, IGCResult::BypassToAct { .. }));
    }

    #[test]
    fn full_deliberation_when_low_confidence() {
        let ooda = temp_ooda();
        let result = ooda.check_igc(0.50);
        assert!(matches!(result, IGCResult::FullDeliberation { .. }));
    }

    #[test]
    fn essential_vars_persistence() {
        let ooda = temp_ooda();
        let vars = EssentialVariables {
            eir: 0.1,
            confidence: 0.8,
            token_budget_used: 0.5,
            latency_ms: 100,
        };
        ooda.update_essential_vars(&vars).unwrap();
        let retrieved = ooda.get_essential_vars().unwrap();
        assert_eq!(retrieved, vars);
    }

    #[test]
    fn ultrastability_trigger_on_token_budget() {
        let ooda = temp_ooda();
        let vars = EssentialVariables {
            token_budget_used: 1.0,
            ..Default::default()
        };
        ooda.update_essential_vars(&vars).unwrap();
        let trigger = ooda.check_ultrastability();
        assert!(matches!(
            trigger,
            Some(UltraStableTrigger::TokenBudgetExhausted)
        ));
    }

    #[test]
    fn cycle_result_contains_all_phases() {
        let ooda = temp_ooda();
        let result = ooda.run_cycle("ação de teste", 0.5).unwrap();
        assert!(result.phase_transitions.contains(&OodaPhase::Observe));
        assert!(result.phase_transitions.contains(&OodaPhase::Orient));
        assert!(result.phase_transitions.contains(&OodaPhase::Decide));
        assert!(result.phase_transitions.contains(&OodaPhase::Act));
        assert!(!result.used_igc_bypass);
        assert_eq!(result.action, "ação de teste");
    }

    #[test]
    fn phase_transition_validation() {
        let ooda = temp_ooda();
        // Observe → Act é inválido diretamente
        assert!(ooda.transition_phase(OodaPhase::Act).is_err());

        // Ciclo válido
        ooda.transition_phase(OodaPhase::Orient).unwrap();
        ooda.transition_phase(OodaPhase::Decide).unwrap();
        ooda.transition_phase(OodaPhase::Act).unwrap();
        ooda.transition_phase(OodaPhase::Observe).unwrap();
        assert_eq!(ooda.current_phase(), OodaPhase::Observe);
    }

    #[test]
    fn config_defaults_are_sensible() {
        let config = OODAConfig::default();
        assert!((config.theta_igc - 0.85).abs() < f64::EPSILON);
        assert!((config.theta_decide - 0.60).abs() < f64::EPSILON);
        assert_eq!(config.max_orient_ms, 500);
        assert_eq!(config.max_decide_ms, 100);
    }

    #[test]
    fn update_essential_vars_roundtrip() {
        let ooda = temp_ooda();
        let vars = EssentialVariables {
            eir: 0.25,
            confidence: 0.75,
            token_budget_used: 0.33,
            latency_ms: 250,
        };
        ooda.update_essential_vars(&vars).unwrap();
        let retrieved = ooda.get_essential_vars().unwrap();
        assert!((retrieved.eir - 0.25).abs() < f64::EPSILON);
        assert!((retrieved.confidence - 0.75).abs() < f64::EPSILON);
        assert!((retrieved.token_budget_used - 0.33).abs() < f64::EPSILON);
        assert_eq!(retrieved.latency_ms, 250);
    }

    #[test]
    fn igc_threshold_configurable() {
        let bb = temp_bb();
        let config = OODAConfig {
            theta_igc: 0.95,
            ..Default::default()
        };
        let ooda = OODACLoop::new(bb, "test-session").with_config(config);
        // 0.90 < 0.95 → deliberação completa
        let result = ooda.check_igc(0.90);
        assert!(matches!(result, IGCResult::FullDeliberation { .. }));
        // 0.96 >= 0.95 → bypass
        let result = ooda.check_igc(0.96);
        assert!(matches!(result, IGCResult::BypassToAct { .. }));
    }

    #[test]
    fn cycle_with_igc_bypass_skips_decide() {
        let ooda = temp_ooda();
        let result = ooda.run_cycle("ação rápida", 0.95).unwrap();
        assert!(result.used_igc_bypass);
        assert!(!result.phase_transitions.contains(&OodaPhase::Decide));
        assert!(result.phase_transitions.contains(&OodaPhase::Observe));
        assert!(result.phase_transitions.contains(&OodaPhase::Orient));
        assert!(result.phase_transitions.contains(&OodaPhase::Act));
    }

    #[test]
    fn fsm_to_ooda_mapping() {
        use crate::AgentState;
        assert_eq!(
            OODACLoop::map_fsm_state(&AgentState::Idle),
            OodaPhase::Observe
        );
        assert_eq!(
            OODACLoop::map_fsm_state(&AgentState::Exploration),
            OodaPhase::Observe
        );
        assert_eq!(
            OODACLoop::map_fsm_state(&AgentState::Planning),
            OodaPhase::Orient
        );
        assert_eq!(
            OODACLoop::map_fsm_state(&AgentState::Correction),
            OodaPhase::Orient
        );
        assert_eq!(
            OODACLoop::map_fsm_state(&AgentState::Evaluation),
            OodaPhase::Decide
        );
        assert_eq!(
            OODACLoop::map_fsm_state(&AgentState::Execution),
            OodaPhase::Act
        );
    }

    #[test]
    fn ultrastability_triggers_error_rate() {
        let ooda = temp_ooda();
        let vars = EssentialVariables {
            eir: 0.6,
            ..Default::default()
        };
        ooda.update_essential_vars(&vars).unwrap();
        let trigger = ooda.check_ultrastability();
        assert!(matches!(
            trigger,
            Some(UltraStableTrigger::ErrorRateTooHigh)
        ));
    }

    #[test]
    fn ultrastability_triggers_latency() {
        let ooda = temp_ooda();
        let vars = EssentialVariables {
            latency_ms: 6000,
            ..Default::default()
        };
        ooda.update_essential_vars(&vars).unwrap();
        let trigger = ooda.check_ultrastability();
        assert!(matches!(
            trigger,
            Some(UltraStableTrigger::LatencySLAViolation)
        ));
    }

    #[test]
    fn ultrastability_triggers_confidence() {
        let ooda = temp_ooda();
        let vars = EssentialVariables {
            confidence: 0.2,
            ..Default::default()
        };
        ooda.update_essential_vars(&vars).unwrap();
        let trigger = ooda.check_ultrastability();
        assert!(matches!(
            trigger,
            Some(UltraStableTrigger::ConfidenceTooLow)
        ));
    }

    #[test]
    fn config_persistence_roundtrip() {
        let ooda = temp_ooda();
        let config = OODAConfig {
            theta_igc: 0.99,
            theta_decide: 0.55,
            max_orient_ms: 1000,
            max_decide_ms: 200,
        };
        let ooda = ooda.with_config(config.clone());
        ooda.save_config().unwrap();
        let loaded = ooda.load_config().unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn sync_with_fsm_sets_correct_phase() {
        let bb = temp_bb();
        let fsm = crate::Fsm::new(bb.clone());
        let ooda = OODACLoop::new(bb, "test-session");

        fsm.transition(crate::AgentState::Exploration).unwrap();
        fsm.transition(crate::AgentState::Planning).unwrap();
        ooda.sync_with_fsm(&fsm).unwrap();
        assert_eq!(ooda.current_phase(), OodaPhase::Orient);

        fsm.transition(crate::AgentState::Execution).unwrap();
        ooda.sync_with_fsm(&fsm).unwrap();
        assert_eq!(ooda.current_phase(), OodaPhase::Act);
    }
}
