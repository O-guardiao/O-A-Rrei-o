//! ContextAssembler — monta frames de contexto conversacional a partir de sessões.
//!
//! Traduzido do ContextAssembler do Agent Memory (SIF v2) e do
//! ContextCompressor do Hermes Agent para a arquitetura O Arreio.
//!
//! Fluxo:
//!   1. Lê mensagens da sessão do Blackboard
//!   2. Lê FrozenSnapshot (system prompt + skills)
//!   3. Verifica budget de tokens
//!   4. Aplica ContextCompressor se > threshold (head/tail protection)
//!   5. Monta SessionContextFrame

use anyhow::Result;
use arreio_kernel::Blackboard;

use crate::context_compressor::{
    CompressorConfig, ContextCompressor, ContextMessage, ContextRole,
};
use crate::frozen_snapshot::FrozenSnapshot;
use crate::project::ProjectMemory;
use crate::recall::RecallPipeline;
use crate::session::{ChatRole, SessionManager};
use crate::session_context::SessionContextFrame;
use crate::sif::SifAssembler;

/// Assembler de contexto conversacional.
/// Produz `SessionContextFrame` a partir de uma sessão persistida.
pub struct ContextAssembler {
    session_mgr: SessionManager,
    compressor: ContextCompressor,
    recall: Option<RecallPipeline>,
    #[allow(dead_code)]
    sif: SifAssembler,
}

impl ContextAssembler {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            session_mgr: SessionManager::new(blackboard.clone()),
            compressor: ContextCompressor::new(CompressorConfig::default()),
            recall: Some(RecallPipeline::new(blackboard.clone())),
            sif: SifAssembler::new(4000), // budget de memória no frame
        }
    }

    /// Monta o frame de contexto para uma sessão.
    ///
    /// # Argumentos
    /// * `session_id` — ID da sessão
    /// * `system_prompt` — prompt de sistema base
    /// * `frozen_snapshot` — snapshot imutável de memória no início da sessão
    /// * `project_memory` — memória durável do projeto (opcional)
    pub fn assemble(
        &mut self,
        session_id: &str,
        system_prompt: &str,
        frozen_snapshot: &FrozenSnapshot,
        _project_memory: Option<&ProjectMemory>,
    ) -> Result<SessionContextFrame> {
        // 1. Carrega mensagens da sessão
        let messages = self.session_mgr.list_messages(session_id)?;

        // 2. Converte para ContextMessage do compressor
        let ctx_messages: Vec<ContextMessage> = messages
            .iter()
            .map(|m| ContextMessage {
                role: chat_role_to_context_role(m.role),
                content: m.content.clone(),
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect();

        // 3. Obtém budget da sessão
        let budget = self.session_mgr.get_budget(session_id)?;

        // 4. Verifica se precisa de compressão
        let (final_messages, summary, removed_count, original_tokens, compressed_tokens) = if self
            .compressor
            .should_compress(&ctx_messages, budget.max_tokens)
        {
            let result = self
                .compressor
                .compress(&ctx_messages, budget.max_tokens, None);
            (
                result.messages,
                result.summary,
                result.removed_count,
                result.original_tokens,
                result.compressed_tokens,
            )
        } else {
            (
                ctx_messages.clone(),
                None,
                0,
                ContextCompressor::estimate_tokens(&ctx_messages),
                ContextCompressor::estimate_tokens(&ctx_messages),
            )
        };

        // 5. Converte mensagens comprimidas de volta para ChatMessage
        let final_chat_messages: Vec<crate::session::ChatMessage> = final_messages
            .iter()
            .enumerate()
            .map(|(i, m)| crate::session::ChatMessage {
                seq: i,
                role: context_role_to_chat_role(m.role),
                content: m.content.clone(),
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
                timestamp: now_epoch_secs(),
                tokens: m.content.len() / 4,
            })
            .collect();

        // 6. Executa recall de memória relevante (última mensagem do usuário como query)
        let memory_refs = if let Some(recall) = &self.recall {
            if let Some(last_user_msg) = messages.iter().rev().find(|m| m.role == ChatRole::User) {
                let results = recall.recall(&last_user_msg.content, 5)?;
                results.into_iter().map(|r| r.memory_id).collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // 7. Monta o frame com mensagens comprimidas (ou originais se não comprimiu)
        let frame = SessionContextFrame {
            session_id: session_id.into(),
            system_prompt: format!(
                "{}\n\n{}",
                system_prompt,
                frozen_snapshot.to_normalized_text()
            ),
            messages: if removed_count > 0 {
                final_chat_messages
            } else {
                messages
            },
            summary,
            removed_count,
            original_tokens,
            compressed_tokens,
            skills_context: Vec::new(), // preenchido externamente
            memory_refs,
            frozen_snapshot_id: None,
        };

        Ok(frame)
    }

    /// Monta frame enxuto apenas com mensagens (sem recall nem project memory).
    /// Usado quando o orçamento de tempo é curto.
    pub fn assemble_fast(
        &mut self,
        session_id: &str,
        system_prompt: &str,
    ) -> Result<SessionContextFrame> {
        let messages = self.session_mgr.list_messages(session_id)?;
        let ctx_messages: Vec<ContextMessage> = messages
            .iter()
            .map(|m| ContextMessage {
                role: chat_role_to_context_role(m.role),
                content: m.content.clone(),
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect();

        let budget = self.session_mgr.get_budget(session_id)?;
        let (final_messages, summary, removed_count, original_tokens, compressed_tokens) = if self
            .compressor
            .should_compress(&ctx_messages, budget.max_tokens)
        {
            let result = self
                .compressor
                .compress(&ctx_messages, budget.max_tokens, None);
            (
                result.messages,
                result.summary,
                result.removed_count,
                result.original_tokens,
                result.compressed_tokens,
            )
        } else {
            (
                ctx_messages.clone(),
                None,
                0,
                ContextCompressor::estimate_tokens(&ctx_messages),
                ContextCompressor::estimate_tokens(&ctx_messages),
            )
        };

        // Converte mensagens comprimidas de volta para ChatMessage
        let final_chat_messages: Vec<crate::session::ChatMessage> = final_messages
            .iter()
            .enumerate()
            .map(|(i, m)| crate::session::ChatMessage {
                seq: i,
                role: context_role_to_chat_role(m.role),
                content: m.content.clone(),
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
                timestamp: now_epoch_secs(),
                tokens: m.content.len() / 4,
            })
            .collect();

        Ok(SessionContextFrame {
            session_id: session_id.into(),
            system_prompt: system_prompt.into(),
            messages: if removed_count > 0 {
                final_chat_messages
            } else {
                messages
            },
            summary,
            removed_count,
            original_tokens,
            compressed_tokens,
            skills_context: Vec::new(),
            memory_refs: Vec::new(),
            frozen_snapshot_id: None,
        })
    }
}

fn chat_role_to_context_role(role: ChatRole) -> ContextRole {
    match role {
        ChatRole::System => ContextRole::System,
        ChatRole::User => ContextRole::User,
        ChatRole::Assistant => ContextRole::Assistant,
        ChatRole::Tool => ContextRole::Tool,
    }
}

fn context_role_to_chat_role(role: ContextRole) -> ChatRole {
    match role {
        ContextRole::System => ChatRole::System,
        ContextRole::User => ChatRole::User,
        ContextRole::Assistant => ChatRole::Assistant,
        ContextRole::Tool => ChatRole::Tool,
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ChatRole, SessionMode};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_assembler() -> ContextAssembler {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        ContextAssembler::new(bb)
    }

    #[test]
    fn assemble_frame_basico() {
        let mut asm = temp_assembler();
        let s = asm
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        asm.session_mgr
            .append_message(&s.id, ChatRole::User, "Olá", None, None, 2)
            .unwrap();
        asm.session_mgr
            .append_message(
                &s.id,
                ChatRole::Assistant,
                "Oi! Como posso ajudar?",
                None,
                None,
                5,
            )
            .unwrap();

        let frame = asm
            .assemble(
                &s.id,
                "Você é um assistente.",
                &FrozenSnapshot::empty(),
                None,
            )
            .unwrap();
        assert_eq!(frame.session_id, s.id);
        assert_eq!(frame.messages.len(), 2);
        assert!(frame.system_prompt.contains("Você é um assistente."));
    }

    #[test]
    fn assemble_fast_sem_recall() {
        let mut asm = temp_assembler();
        let s = asm
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        asm.session_mgr
            .append_message(&s.id, ChatRole::User, "test", None, None, 1)
            .unwrap();

        let frame = asm.assemble_fast(&s.id, "System prompt").unwrap();
        assert_eq!(frame.messages.len(), 1);
        assert!(frame.memory_refs.is_empty());
    }
}
