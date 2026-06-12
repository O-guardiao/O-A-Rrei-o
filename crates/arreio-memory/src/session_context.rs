//! Session Context — estruturas de controle de lifecycle e frame de contexto
//! para sessões conversacionais.
//!
//! O `SessionContextFrame` é montado pelo `ContextAssembler` (Fase 2) a partir
//! das mensagens da sessão + FrozenSnapshot + RecallPipeline.

use serde::{Deserialize, Serialize};

/// Estado de lifecycle de uma sessão conversacional.
/// Traduzido do GoalLifecycleStore do Agent Memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionLifecycleState {
    Active,
    Paused,
    BudgetLimited,
    Complete,
}

impl std::fmt::Display for SessionLifecycleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionLifecycleState::Active => write!(f, "active"),
            SessionLifecycleState::Paused => write!(f, "paused"),
            SessionLifecycleState::BudgetLimited => write!(f, "budget_limited"),
            SessionLifecycleState::Complete => write!(f, "complete"),
        }
    }
}

/// Controle anti-loop para sessões conversacionais.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionAntiLoop {
    pub consecutive_no_tool_turns: u32,
    pub continuation_suppressed: bool,
    pub threshold: u32,
}

impl Default for SessionAntiLoop {
    fn default() -> Self {
        Self {
            consecutive_no_tool_turns: 0,
            continuation_suppressed: false,
            threshold: 3,
        }
    }
}

impl SessionAntiLoop {
    pub fn record_turn(&mut self, had_tool_calls: bool) {
        if had_tool_calls {
            self.consecutive_no_tool_turns = 0;
            self.continuation_suppressed = false;
        } else {
            self.consecutive_no_tool_turns += 1;
            if self.consecutive_no_tool_turns >= self.threshold {
                self.continuation_suppressed = true;
            }
        }
    }

    pub fn is_suppressed(&self) -> bool {
        self.continuation_suppressed
    }
}

/// Frame de contexto montado para envio ao ator.
/// Produzido pelo ContextAssembler na Fase 2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContextFrame {
    pub session_id: String,
    pub system_prompt: String,
    pub messages: Vec<super::session::ChatMessage>,
    pub summary: Option<String>,
    pub removed_count: usize,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub skills_context: Vec<String>,
    pub memory_refs: Vec<String>,
    pub frozen_snapshot_id: Option<String>,
}

impl SessionContextFrame {
    /// Estima tokens totais do frame (heurística chars/4).
    pub fn estimate_tokens(&self) -> usize {
        let system_tokens = self.system_prompt.len() / 4;
        let msg_tokens: usize = self.messages.iter().map(|m| m.content.len() / 4).sum();
        let summary_tokens = self.summary.as_ref().map(|s| s.len() / 4).unwrap_or(0);
        system_tokens + msg_tokens + summary_tokens
    }

    /// Verifica se o frame foi comprimido.
    pub fn was_compressed(&self) -> bool {
        self.removed_count > 0 || self.summary.is_some()
    }
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anti_loop_suprime_apos_threshold() {
        let mut anti = SessionAntiLoop::default();
        assert!(!anti.is_suppressed());

        anti.record_turn(false);
        anti.record_turn(false);
        assert!(!anti.is_suppressed());

        anti.record_turn(false);
        assert!(anti.is_suppressed());
    }

    #[test]
    fn anti_loop_reseta_com_tool() {
        let mut anti = SessionAntiLoop::default();
        anti.record_turn(false);
        anti.record_turn(false);
        anti.record_turn(true);
        assert!(!anti.is_suppressed());
        assert_eq!(anti.consecutive_no_tool_turns, 0);
    }
}
