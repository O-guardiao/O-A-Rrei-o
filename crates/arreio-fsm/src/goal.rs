use anyhow::{bail, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};

// ── Tipos ─────────────────────────────────────────────────────────────────────

/// Status de um goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    Active,
    Paused,
    Done,
    Cleared,
}

/// Subgoal com critério de verificação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subgoal {
    pub text: String,
    pub evidence: Option<String>,
    pub verified: bool,
}

/// Estado completo de um goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub text: String,
    pub status: GoalStatus,
    pub turns_used: u32,
    pub max_turns: u32,
    pub subgoals: Vec<Subgoal>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Número de falhas consecutivas de parse do judge.
    pub consecutive_parse_failures: u32,
}

impl Goal {
    pub fn new(text: impl Into<String>, max_turns: u32) -> Self {
        let now = now_epoch_secs();
        Self {
            text: text.into(),
            status: GoalStatus::Active,
            turns_used: 0,
            max_turns,
            subgoals: Vec::new(),
            created_at: now,
            updated_at: now,
            consecutive_parse_failures: 0,
        }
    }

    pub fn add_subgoal(&mut self, text: impl Into<String>) {
        self.subgoals.push(Subgoal {
            text: text.into(),
            evidence: None,
            verified: false,
        });
        self.updated_at = now_epoch_secs();
    }

    pub fn remove_subgoal(&mut self, index: usize) -> Result<()> {
        if index >= self.subgoals.len() {
            bail!(
                "índice de subgoal inválido: {} (total: {})",
                index,
                self.subgoals.len()
            );
        }
        self.subgoals.remove(index);
        self.updated_at = now_epoch_secs();
        Ok(())
    }

    pub fn budget_exhausted(&self) -> bool {
        self.turns_used >= self.max_turns
    }

    pub fn all_subgoals_verified(&self) -> bool {
        self.subgoals.is_empty() || self.subgoals.iter().all(|s| s.verified)
    }
}

// ── Judge Result ──────────────────────────────────────────────────────────────

/// Resultado da avaliação do Judge LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeVerdict {
    Done { reason: String },
    Continue { reason: String },
    GenericRejection { reason: String },
}

/// Resposta bruta do judge — pode falhar parse.
#[derive(Debug, Clone)]
pub struct JudgeResult {
    pub verdict: Option<JudgeVerdict>,
    pub raw_response: String,
    pub parse_error: Option<String>,
}

// ── Judge Client Trait ────────────────────────────────────────────────────────

/// Abstração sobre o LLM usado como Judge.
/// Permite mock em testes e integração real com ProviderClient.
pub trait JudgeClient: Send + Sync {
    /// Envia prompt de judge e retorna resposta bruta.
    fn ask(&self, system_prompt: &str, user_prompt: &str) -> Result<String>;
}

/// Implementação mock para testes.
pub struct MockJudgeClient {
    responses: Arc<Mutex<Vec<String>>>,
    call_count: Arc<Mutex<usize>>,
}

impl MockJudgeClient {
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    pub fn call_count(&self) -> usize {
        *self.call_count.lock().unwrap()
    }
}

impl JudgeClient for MockJudgeClient {
    fn ask(&self, _system_prompt: &str, _user_prompt: &str) -> Result<String> {
        let mut count = self.call_count.lock().unwrap();
        let responses = self.responses.lock().unwrap();
        let idx = *count;
        *count += 1;
        if idx < responses.len() {
            Ok(responses[idx].clone())
        } else {
            bail!("MockJudgeClient sem mais respostas")
        }
    }
}

// ── Judge Config ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct JudgeConfig {
    pub max_consecutive_parse_failures: u32,
    pub goal_max_chars: usize,
    pub response_max_chars: usize,
    pub system_prompt: String,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            max_consecutive_parse_failures: 3,
            goal_max_chars: 2000,
            response_max_chars: 4000,
            system_prompt: concat!(
                "You are a goal-completion judge. Evaluate whether the assistant has completed the user's goal.\n",
                "Respond with valid JSON only: {\"done\": true|false, \"reason\": \"string\"}.\n",
                "If subgoals exist, done=true only if ALL subgoals have concrete evidence.\n",
                "Reject generic claims like 'all requirements met' — demand specific evidence."
            )
            .to_string(),
        }
    }
}

// ── Goal Manager ──────────────────────────────────────────────────────────────

/// Gerencia goals no Blackboard com judge loop.
pub struct GoalManager {
    blackboard: Blackboard,
    session_id: String,
    config: JudgeConfig,
}

impl GoalManager {
    pub fn new(blackboard: Blackboard, session_id: impl Into<String>) -> Self {
        Self {
            blackboard,
            session_id: session_id.into(),
            config: JudgeConfig::default(),
        }
    }

    pub fn with_config(mut self, config: JudgeConfig) -> Self {
        self.config = config;
        self
    }

    fn bb_key(&self) -> String {
        format!("goal:{}", self.session_id)
    }

    pub fn set_goal(&self, text: impl Into<String>, max_turns: u32) -> Result<()> {
        let goal = Goal::new(text, max_turns);
        self.save_goal(&goal)
    }

    pub fn get_goal(&self) -> Option<Goal> {
        self.blackboard
            .get_tuple("goal", &self.bb_key())
            .and_then(|v| serde_json::from_value(v).ok())
    }

    pub fn update_goal(&self, goal: &Goal) -> Result<()> {
        self.save_goal(goal)
    }

    fn save_goal(&self, goal: &Goal) -> Result<()> {
        let value = serde_json::to_value(goal)?;
        self.blackboard.put_tuple("goal", &self.bb_key(), value)
    }

    /// Define ou atualiza o goal.
    pub fn define_goal(&self, text: impl Into<String>, max_turns: u32) -> Result<()> {
        self.set_goal(text, max_turns)
    }

    /// Pausa o goal atual.
    pub fn pause(&self) -> Result<()> {
        if let Some(mut goal) = self.get_goal() {
            goal.status = GoalStatus::Paused;
            self.save_goal(&goal)
        } else {
            bail!("nenhum goal definido")
        }
    }

    /// Resume o goal pausado.
    pub fn resume(&self) -> Result<()> {
        if let Some(mut goal) = self.get_goal() {
            goal.status = GoalStatus::Active;
            self.save_goal(&goal)
        } else {
            bail!("nenhum goal definido")
        }
    }

    /// Limpa o goal.
    pub fn clear(&self) -> Result<()> {
        if let Some(mut goal) = self.get_goal() {
            goal.status = GoalStatus::Cleared;
            self.save_goal(&goal)
        } else {
            bail!("nenhum goal definido")
        }
    }

    /// Status do goal.
    pub fn status(&self) -> Option<GoalStatus> {
        self.get_goal().map(|g| g.status.clone())
    }

    /// Adiciona um subgoal.
    pub fn add_subgoal(&self, text: impl Into<String>) -> Result<()> {
        if let Some(mut goal) = self.get_goal() {
            goal.add_subgoal(text);
            self.save_goal(&goal)
        } else {
            bail!("nenhum goal definido")
        }
    }

    /// Remove um subgoal por índice.
    pub fn remove_subgoal(&self, index: usize) -> Result<()> {
        if let Some(mut goal) = self.get_goal() {
            goal.remove_subgoal(index)?;
            self.save_goal(&goal)
        } else {
            bail!("nenhum goal definido")
        }
    }

    /// Incrementa o contador de turns usados.
    pub fn record_turn(&self) -> Result<()> {
        if let Some(mut goal) = self.get_goal() {
            goal.turns_used += 1;
            goal.updated_at = now_epoch_secs();
            self.save_goal(&goal)
        } else {
            bail!("nenhum goal definido")
        }
    }

    /// Avalia se deve continuar automaticamente após um turno.
    /// Retorna true se deve continuar, false se deve parar.
    pub fn should_auto_continue(&self) -> bool {
        let Some(goal) = self.get_goal() else {
            return false;
        };
        matches!(goal.status, GoalStatus::Active)
            && !goal.budget_exhausted()
            && !matches!(goal.status, GoalStatus::Done | GoalStatus::Cleared)
    }

    /// Executa o judge loop após um turno.
    /// Retorna o resultado do judge.
    pub fn judge_turn(&self, last_response: &str, judge: &dyn JudgeClient) -> Result<JudgeResult> {
        let Some(mut goal) = self.get_goal() else {
            bail!("nenhum goal definido para avaliar");
        };

        if !matches!(goal.status, GoalStatus::Active) {
            return Ok(JudgeResult {
                verdict: None,
                raw_response: "".to_string(),
                parse_error: Some("goal não está ativo".to_string()),
            });
        }

        // Trunca inputs
        let goal_text = truncate(&goal.text, self.config.goal_max_chars);
        let response_text = truncate(last_response, self.config.response_max_chars);

        // Constrói prompt de subgoals
        let subgoals_text = if goal.subgoals.is_empty() {
            "No subgoals defined.".to_string()
        } else {
            let mut lines = vec!["Subgoals:".to_string()];
            for (i, sg) in goal.subgoals.iter().enumerate() {
                let status = if sg.verified { "✓" } else { "○" };
                lines.push(format!("{} {}: {}", status, i + 1, sg.text));
            }
            lines.join("\n")
        };

        let user_prompt = format!(
            "Goal: {}\n\n{}\n\nAssistant response (truncated):\n{}\n\nEvaluate: is the goal completed? Respond ONLY with JSON {{\"done\": bool, \"reason\": \"string\"}}.",
            goal_text, subgoals_text, response_text
        );

        let raw_response = judge.ask(&self.config.system_prompt, &user_prompt)?;
        let parsed = Self::parse_judge_response(&raw_response, &goal.subgoals);

        let mut result = JudgeResult {
            verdict: None,
            raw_response: raw_response.clone(),
            parse_error: None,
        };

        match parsed {
            Ok(verdict) => {
                goal.consecutive_parse_failures = 0;
                match &verdict {
                    JudgeVerdict::Done { reason: _ } => {
                        // Verifica se há subgoals não verificados
                        if goal.all_subgoals_verified() {
                            goal.status = GoalStatus::Done;
                        } else {
                            // Subgoals pendentes → não marcar done
                            result.verdict = Some(JudgeVerdict::GenericRejection {
                                reason: format!(
                                    "subgoals pendentes: {}",
                                    goal.subgoals.iter().filter(|s| !s.verified).count()
                                ),
                            });
                            self.save_goal(&goal)?;
                            return Ok(result);
                        }
                    }
                    JudgeVerdict::Continue { .. } => {
                        // Verifica budget
                        if goal.budget_exhausted() {
                            goal.status = GoalStatus::Paused;
                        }
                    }
                    _ => {}
                }
                result.verdict = Some(verdict);
            }
            Err(e) => {
                goal.consecutive_parse_failures += 1;
                result.parse_error = Some(e.to_string());
                // Auto-pause após N falhas
                if goal.consecutive_parse_failures >= self.config.max_consecutive_parse_failures {
                    goal.status = GoalStatus::Paused;
                }
            }
        }

        goal.updated_at = now_epoch_secs();
        self.save_goal(&goal)?;
        Ok(result)
    }

    fn parse_judge_response(raw: &str, subgoals: &[Subgoal]) -> Result<JudgeVerdict> {
        // Tenta extrair JSON
        let json_str = Self::extract_json(raw);
        let value: Value = serde_json::from_str(&json_str)?;

        let done = value
            .get("done")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| anyhow::anyhow!("campo 'done' ausente ou não booleano"))?;

        let reason = value
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("no reason")
            .to_string();

        // Rejeição genérica: se há subgoals mas a resposta é genérica
        if !subgoals.is_empty() && done {
            let lower = reason.to_lowercase();
            let generic_phrases = [
                "all requirements met",
                "all done",
                "completed successfully",
                "task finished",
                "everything is done",
            ];
            if generic_phrases.iter().any(|p| lower.contains(p)) {
                return Ok(JudgeVerdict::GenericRejection {
                    reason: "generic claim without subgoal evidence".to_string(),
                });
            }
        }

        if done {
            Ok(JudgeVerdict::Done { reason })
        } else {
            Ok(JudgeVerdict::Continue { reason })
        }
    }

    fn extract_json(text: &str) -> String {
        // Procura por JSON entre ```json ... ``` ou chaves diretas
        if let Some(start) = text.find("```json") {
            if let Some(end) = text[start + 7..].find("```") {
                return text[start + 7..start + 7 + end].trim().to_string();
            }
        }
        if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                return text[start..=end].to_string();
            }
        }
        text.to_string()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let mut end = max_chars;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... [truncated]", &s[..end])
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

    fn temp_manager() -> GoalManager {
        GoalManager::new(temp_bb(), "test-session")
    }

    // ── CRUD básico ───────────────────────────────────────────────────────────

    #[test]
    fn define_and_get_goal() {
        let mgr = temp_manager();
        mgr.define_goal("Write a Rust module", 10).unwrap();
        let goal = mgr.get_goal().unwrap();
        assert_eq!(goal.text, "Write a Rust module");
        assert_eq!(goal.max_turns, 10);
        assert_eq!(goal.turns_used, 0);
        assert!(matches!(goal.status, GoalStatus::Active));
    }

    #[test]
    fn get_goal_returns_none_when_undefined() {
        let mgr = temp_manager();
        assert!(mgr.get_goal().is_none());
    }

    #[test]
    fn pause_resume_clear() {
        let mgr = temp_manager();
        mgr.define_goal("Test goal", 5).unwrap();

        mgr.pause().unwrap();
        assert!(matches!(mgr.status().unwrap(), GoalStatus::Paused));

        mgr.resume().unwrap();
        assert!(matches!(mgr.status().unwrap(), GoalStatus::Active));

        mgr.clear().unwrap();
        assert!(matches!(mgr.status().unwrap(), GoalStatus::Cleared));
    }

    #[test]
    fn pause_without_goal_errors() {
        let mgr = temp_manager();
        assert!(mgr.pause().is_err());
    }

    // ── Subgoals ──────────────────────────────────────────────────────────────

    #[test]
    fn add_and_remove_subgoals() {
        let mgr = temp_manager();
        mgr.define_goal("Build API", 10).unwrap();
        mgr.add_subgoal("Define routes").unwrap();
        mgr.add_subgoal("Add tests").unwrap();

        let goal = mgr.get_goal().unwrap();
        assert_eq!(goal.subgoals.len(), 2);
        assert_eq!(goal.subgoals[0].text, "Define routes");

        mgr.remove_subgoal(0).unwrap();
        let goal = mgr.get_goal().unwrap();
        assert_eq!(goal.subgoals.len(), 1);
        assert_eq!(goal.subgoals[0].text, "Add tests");
    }

    #[test]
    fn remove_invalid_subgoal_errors() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 5).unwrap();
        assert!(mgr.remove_subgoal(0).is_err());
    }

    // ── Budget ────────────────────────────────────────────────────────────────

    #[test]
    fn budget_exhaustion() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 3).unwrap();
        mgr.record_turn().unwrap();
        mgr.record_turn().unwrap();
        mgr.record_turn().unwrap();

        let goal = mgr.get_goal().unwrap();
        assert!(goal.budget_exhausted());
    }

    // ── Judge Loop ────────────────────────────────────────────────────────────

    #[test]
    fn judge_done_sets_goal_done() {
        let mgr = temp_manager();
        mgr.define_goal("Write hello world", 5).unwrap();

        let judge = MockJudgeClient::new(vec![
            r#"{"done": true, "reason": "code compiles"}"#.to_string()
        ]);
        let result = mgr
            .judge_turn("fn main() { println!(\"hello\"); }", &judge)
            .unwrap();

        assert!(result.verdict.is_some());
        let goal = mgr.get_goal().unwrap();
        assert!(matches!(goal.status, GoalStatus::Done));
    }

    #[test]
    fn judge_continue_keeps_active() {
        let mgr = temp_manager();
        mgr.define_goal("Build large module", 10).unwrap();

        let judge = MockJudgeClient::new(vec![
            r#"{"done": false, "reason": "needs more work"}"#.to_string()
        ]);
        let result = mgr.judge_turn("partial code...", &judge).unwrap();

        assert!(result.verdict.is_some());
        let goal = mgr.get_goal().unwrap();
        assert!(matches!(goal.status, GoalStatus::Active));
    }

    #[test]
    fn judge_generic_rejection_with_subgoals() {
        let mgr = temp_manager();
        mgr.define_goal("Build API", 10).unwrap();
        mgr.add_subgoal("Add auth").unwrap();

        let judge = MockJudgeClient::new(vec![
            r#"{"done": true, "reason": "all requirements met"}"#.to_string(),
        ]);
        let result = mgr.judge_turn("done!", &judge).unwrap();

        assert!(matches!(
            result.verdict,
            Some(JudgeVerdict::GenericRejection { .. })
        ));
        let goal = mgr.get_goal().unwrap();
        // Ainda não done porque subgoals pendentes
        assert!(!matches!(goal.status, GoalStatus::Done));
    }

    #[test]
    fn judge_parse_failure_increments_counter() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 10).unwrap();

        let judge = MockJudgeClient::new(vec!["not json at all".to_string()]);
        let result = mgr.judge_turn("response", &judge).unwrap();

        assert!(result.parse_error.is_some());
        let goal = mgr.get_goal().unwrap();
        assert_eq!(goal.consecutive_parse_failures, 1);
    }

    #[test]
    fn judge_auto_pause_after_max_failures() {
        let _mgr = temp_manager();
        let config = JudgeConfig {
            max_consecutive_parse_failures: 2,
            ..Default::default()
        };
        let mgr = GoalManager::new(temp_bb(), "test-session").with_config(config);
        mgr.define_goal("Test", 10).unwrap();

        let judge = MockJudgeClient::new(vec!["bad".to_string(), "also bad".to_string()]);

        mgr.judge_turn("r1", &judge).unwrap();
        let goal = mgr.get_goal().unwrap();
        assert!(matches!(goal.status, GoalStatus::Active));
        assert_eq!(goal.consecutive_parse_failures, 1);

        mgr.judge_turn("r2", &judge).unwrap();
        let goal = mgr.get_goal().unwrap();
        assert!(matches!(goal.status, GoalStatus::Paused));
        assert_eq!(goal.consecutive_parse_failures, 2);
    }

    #[test]
    fn judge_extracts_json_from_codeblock() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 5).unwrap();

        let judge = MockJudgeClient::new(vec![
            "Here is my evaluation:\n```json\n{\"done\": true, \"reason\": \"done\"}\n```"
                .to_string(),
        ]);
        let result = mgr.judge_turn("done", &judge).unwrap();

        assert!(matches!(result.verdict, Some(JudgeVerdict::Done { .. })));
    }

    #[test]
    fn should_auto_continue_active_with_budget() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 5).unwrap();
        assert!(mgr.should_auto_continue());

        mgr.pause().unwrap();
        assert!(!mgr.should_auto_continue());
    }

    #[test]
    fn should_auto_continue_false_when_no_goal() {
        let _mgr = temp_manager();
        assert!(!_mgr.should_auto_continue());
    }

    #[test]
    fn should_auto_continue_false_when_budget_exhausted() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 1).unwrap();
        mgr.record_turn().unwrap();
        assert!(!mgr.should_auto_continue());
    }

    #[test]
    fn judge_with_subgoals_all_verified_then_done() {
        let mgr = temp_manager();
        mgr.define_goal("Build API", 5).unwrap();
        mgr.add_subgoal("Add routes").unwrap();
        mgr.add_subgoal("Add tests").unwrap();

        // Pre-marca subgoals como verificados (simulando verificação externa)
        let mut goal = mgr.get_goal().unwrap();
        goal.subgoals[0].verified = true;
        goal.subgoals[1].verified = true;
        mgr.update_goal(&goal).unwrap();

        let judge = MockJudgeClient::new(vec![
            r#"{"done": true, "reason": "both subgoals verified"}"#.to_string(),
        ]);
        let result = mgr.judge_turn("done", &judge).unwrap();

        assert!(matches!(result.verdict, Some(JudgeVerdict::Done { .. })));
        let goal = mgr.get_goal().unwrap();
        assert!(matches!(goal.status, GoalStatus::Done));
    }

    #[test]
    fn judge_ignores_inactive_goal() {
        let mgr = temp_manager();
        mgr.define_goal("Test", 5).unwrap();
        mgr.pause().unwrap();

        let judge = MockJudgeClient::new(vec![r#"{"done": true, "reason": "x"}"#.to_string()]);
        let result = mgr.judge_turn("r", &judge).unwrap();

        assert!(result.parse_error.is_some());
        assert!(result.verdict.is_none());
    }

    #[test]
    fn truncate_long_strings() {
        let s = "a".repeat(3000);
        let truncated = truncate(&s, 2000);
        assert!(truncated.len() <= 2020); // 2000 + "... [truncated]"
        assert!(truncated.ends_with("... [truncated]"));
    }

    #[test]
    fn goal_serialization_roundtrip() {
        let goal = Goal::new("Test goal", 10);
        let json = serde_json::to_string(&goal).unwrap();
        let restored: Goal = serde_json::from_str(&json).unwrap();
        assert_eq!(goal.text, restored.text);
        assert_eq!(goal.max_turns, restored.max_turns);
    }
}
