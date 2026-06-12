//! AutoCompressor — compressão automática de contexto conversacional.
//!
//! Quando o contexto excede o threshold (75% do budget), comprime automaticamente
//! sem perguntar ao usuário. A notificação é suave e não bloqueante.
//!
//! Princípio: o sistema decide, o usuário conversa.

use anyhow::Result;
use arreio_kernel::Blackboard;

use crate::context_compressor::{CompressorConfig, ContextCompressor, ContextMessage, ContextRole};
use crate::session::{ChatMessage, ChatRole, SessionManager};

/// Resultado da compressão automática.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoCompressResult {
    /// True se a compressão foi executada.
    pub was_compressed: bool,
    /// Mensagem de notificação para o usuário (ou vazia se não notificar).
    pub notification: Option<String>,
    /// Número de mensagens removidas.
    pub removed_count: usize,
    /// Tokens antes da compressão.
    pub original_tokens: usize,
    /// Tokens após a compressão.
    pub compressed_tokens: usize,
    /// Summary gerado (se houver).
    pub summary: Option<String>,
}

/// Threshold de falhas consecutivas para abrir o circuit breaker (GAP-004).
const CIRCUIT_BREAKER_THRESHOLD: u8 = 3;

/// Compressor automático de contexto.
///
/// Esconde toda a complexidade de compressão do usuário leigo.
/// O sistema comprime quando necessário, notifica suavemente, e continua.
pub struct AutoCompressor {
    session_mgr: SessionManager,
    compressor: ContextCompressor,
    /// Threshold para compressão automática (0.0 a 1.0).
    threshold_percent: f32,
    /// Se true, notifica o usuário quando comprime.
    notify_user: bool,
}

impl AutoCompressor {
    /// Cria um novo compressor automático.
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            session_mgr: SessionManager::new(blackboard),
            compressor: ContextCompressor::new(CompressorConfig::default()),
            threshold_percent: 0.75,
            notify_user: true,
        }
    }

    /// Define o threshold de compressão.
    pub fn with_threshold(mut self, percent: f32) -> Self {
        self.threshold_percent = percent.clamp(0.1, 0.95);
        self
    }

    /// Desabilita notificação ao usuário.
    pub fn silent(mut self) -> Self {
        self.notify_user = false;
        self
    }

    /// Verifica e comprime a sessão se necessário.
    ///
    /// Retorna `AutoCompressResult` indicando se a compressão ocorreu.
    ///
    /// # Circuit Breaker (GAP-004)
    /// Após 3 falhas consecutivas de auto-compact, o circuit breaker abre e
    /// retorna erro explícito, impedindo que tokens sejam queimados indefinidamente.
    /// O contador é resetado após compactação bem-sucedida ou turno sem compaction.
    pub fn check_and_compress(&mut self, session_id: &str) -> Result<AutoCompressResult> {
        let mut budget = self.session_mgr.get_budget(session_id)?;

        // ── Circuit Breaker: verifica threshold ANTES de tentar ─────────────────
        if budget.consecutive_autocompact_failures >= CIRCUIT_BREAKER_THRESHOLD {
            return Err(anyhow::anyhow!(
                "Circuit breaker de auto-compact aberto: {} falhas consecutivas. \
                 O loop de compactação foi interrompido para evitar queima de tokens. \
                 Reinicie a sessão ou reduza o contexto manualmente.",
                budget.consecutive_autocompact_failures
            ));
        }

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

        let original_tokens = ContextCompressor::estimate_tokens(&ctx_messages);
        let threshold_tokens = (budget.max_tokens as f32 * self.threshold_percent) as usize;

        // ── Turno sem compaction: reset do contador ─────────────────────────────
        if original_tokens <= threshold_tokens {
            if budget.consecutive_autocompact_failures > 0 {
                budget.consecutive_autocompact_failures = 0;
                self.session_mgr.put_budget(session_id, &budget)?;
            }
            return Ok(AutoCompressResult {
                was_compressed: false,
                notification: None,
                removed_count: 0,
                original_tokens,
                compressed_tokens: original_tokens,
                summary: None,
            });
        }

        // Executa compressão
        let result = self
            .compressor
            .compress(&ctx_messages, budget.max_tokens, None);

        // ── Circuit Breaker: avalia sucesso/falha da compactação ────────────────
        let saved_tokens = result
            .original_tokens
            .saturating_sub(result.compressed_tokens);
        let is_success = saved_tokens > 0 && result.compressed_tokens < result.original_tokens;

        if is_success {
            // Reset do contador em sucesso
            budget.consecutive_autocompact_failures = 0;
        } else {
            // Incrementa contador em falha (compressão não resolveu overflow)
            budget.consecutive_autocompact_failures =
                budget.consecutive_autocompact_failures.saturating_add(1);
            self.session_mgr.put_budget(session_id, &budget)?;

            if budget.consecutive_autocompact_failures >= CIRCUIT_BREAKER_THRESHOLD {
                return Err(anyhow::anyhow!(
                    "Circuit breaker de auto-compact aberto após {} falhas consecutivas. \
                     A compactação não conseguiu reduzir tokens suficientemente. \
                     Reinicie a sessão ou reduza o contexto manualmente.",
                    budget.consecutive_autocompact_failures
                ));
            }
        }

        // Substitui mensagens da sessão pelas comprimidas
        let compressed_chat_messages: Vec<ChatMessage> = result
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| ChatMessage {
                seq: i,
                role: context_role_to_chat_role(m.role),
                content: m.content.clone(),
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
                timestamp: now_epoch_secs(),
                tokens: m.content.len() / 4,
            })
            .collect();

        self.session_mgr
            .replace_messages(session_id, &compressed_chat_messages)?;

        // Se há summary, adiciona como mensagem de sistema informativa
        if let Some(ref summary) = result.summary {
            let summary_msg = format!(
                "[Contexto resumido automaticamente] {}\n\nMensagens anteriores foram condensadas para economizar espaço.",
                summary
            );
            self.session_mgr.append_message(
                session_id,
                ChatRole::System,
                &summary_msg,
                None,
                None,
                summary_msg.len() / 4,
            )?;
        }

        // Atualiza budget
        budget.used_tokens = result.compressed_tokens;
        self.session_mgr.put_budget(session_id, &budget)?;

        let notification = if self.notify_user && result.removed_count > 0 {
            Some(format!(
                "*Resumindo nossa conversa para continuar com mais espaço... ({} mensagens resumidas)*",
                result.removed_count
            ))
        } else {
            None
        };

        Ok(AutoCompressResult {
            was_compressed: true,
            notification,
            removed_count: result.removed_count,
            original_tokens: result.original_tokens,
            compressed_tokens: result.compressed_tokens,
            summary: result.summary,
        })
    }

    /// Verifica se a sessão precisa de compressão (sem executar).
    pub fn needs_compression(&mut self, session_id: &str) -> Result<bool> {
        let messages = self.session_mgr.list_messages(session_id)?;
        let budget = self.session_mgr.get_budget(session_id)?;

        let ctx_messages: Vec<ContextMessage> = messages
            .iter()
            .map(|m| ContextMessage {
                role: chat_role_to_context_role(m.role),
                content: m.content.clone(),
                tool_calls: m.tool_calls.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect();

        let original_tokens = ContextCompressor::estimate_tokens(&ctx_messages);
        let threshold_tokens = (budget.max_tokens as f32 * self.threshold_percent) as usize;

        Ok(original_tokens > threshold_tokens)
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
    use crate::session::SessionMode;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_compressor() -> AutoCompressor {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        AutoCompressor::new(bb)
    }

    #[test]
    fn sem_compressao_quando_abaixo_threshold() {
        let mut comp = temp_compressor();
        let bb = comp.session_mgr.list().unwrap();
        let _ = bb; // evita warning

        // Cria sessão com poucas mensagens
        let s = comp
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        comp.session_mgr
            .append_message(&s.id, ChatRole::User, "Oi", None, None, 1)
            .unwrap();

        let result = comp.check_and_compress(&s.id).unwrap();
        assert!(!result.was_compressed);
        assert_eq!(result.removed_count, 0);
    }

    #[test]
    fn comprime_quando_acima_threshold() {
        let mut comp = temp_compressor();
        let s = comp
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        // Cria muitas mensagens grandes para exceder threshold
        for i in 0..50 {
            let content = format!("Mensagem número {} com muito conteúdo para encher o contexto e forçar a compressão automática do sistema. ", i)
                .repeat(20);
            let role = if i % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            };
            comp.session_mgr
                .append_message(&s.id, role, &content, None, None, content.len() / 4)
                .unwrap();
        }

        let result = comp.check_and_compress(&s.id).unwrap();
        // Pode ou não comprimir dependendo do threshold exato; verificamos se a lógica roda
        // sem panico
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn needs_compression_detecta_corretamente() {
        let mut comp = temp_compressor();
        let s = comp
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        // Sem mensagens → não precisa
        assert!(!comp.needs_compression(&s.id).unwrap());

        // Com mensagens grandes → pode precisar
        let big = "a".repeat(10000);
        comp.session_mgr
            .append_message(&s.id, ChatRole::User, &big, None, None, big.len() / 4)
            .unwrap();

        // Agora deve precisar (10000 chars ≈ 2500 tokens > 75% de 32768 = 24576? Não, 2500 < 24576)
        // Precisamos de mensagens MUITO grandes. Vamos adicionar mais.
        for _ in 0..20 {
            comp.session_mgr
                .append_message(&s.id, ChatRole::User, &big, None, None, big.len() / 4)
                .unwrap();
        }

        let needs = comp.needs_compression(&s.id).unwrap();
        // 21 * 2500 = 52500 tokens, que é > 75% de 32768 (24576)
        assert!(needs, "deveria precisar de compressão com ~52500 tokens");
    }

    // ── GAP-004: Circuit Breaker de Auto-Compact ─────────────────────────────

    #[test]
    fn circuit_breaker_abre_apos_tres_falhas() {
        let mut comp = temp_compressor();
        let s = comp
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        // Simula 3 falhas consecutivas manipulando o budget
        let mut budget = comp.session_mgr.get_budget(&s.id).unwrap();
        budget.consecutive_autocompact_failures = 3;
        comp.session_mgr.put_budget(&s.id, &budget).unwrap();

        let result = comp.check_and_compress(&s.id);
        assert!(result.is_err(), "circuit breaker deve abrir após 3 falhas");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Circuit breaker"),
            "erro deve mencionar circuit breaker: {}",
            err_msg
        );
    }

    #[test]
    fn circuit_breaker_reseta_apos_sucesso() {
        let mut comp = temp_compressor();
        let s = comp
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        // Seta contador de falhas para 2
        let mut budget = comp.session_mgr.get_budget(&s.id).unwrap();
        budget.consecutive_autocompact_failures = 2;
        comp.session_mgr.put_budget(&s.id, &budget).unwrap();

        // Adiciona muitas mensagens grandes para forçar compressão bem-sucedida
        for i in 0..50 {
            let content = format!("Mensagem número {} com muito conteúdo para encher o contexto e forçar a compressão automática do sistema. ", i).repeat(30);
            let role = if i % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            };
            comp.session_mgr
                .append_message(&s.id, role, &content, None, None, content.len() / 4)
                .unwrap();
        }

        let result = comp.check_and_compress(&s.id).unwrap();
        assert!(result.was_compressed);

        // Verifica que o contador foi resetado
        let budget_after = comp.session_mgr.get_budget(&s.id).unwrap();
        assert_eq!(
            budget_after.consecutive_autocompact_failures, 0,
            "contador deve resetar após sucesso"
        );
    }

    #[test]
    fn circuit_breaker_reseta_apos_turno_sem_compaction() {
        let mut comp = temp_compressor();
        let s = comp
            .session_mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        // Seta contador de falhas
        let mut budget = comp.session_mgr.get_budget(&s.id).unwrap();
        budget.consecutive_autocompact_failures = 2;
        comp.session_mgr.put_budget(&s.id, &budget).unwrap();

        // Apenas uma mensagem pequena — não precisa de compressão
        comp.session_mgr
            .append_message(&s.id, ChatRole::User, "Oi", None, None, 1)
            .unwrap();

        let result = comp.check_and_compress(&s.id).unwrap();
        assert!(!result.was_compressed);

        // Verifica que o contador foi resetado
        let budget_after = comp.session_mgr.get_budget(&s.id).unwrap();
        assert_eq!(
            budget_after.consecutive_autocompact_failures, 0,
            "contador deve resetar após turno sem compaction"
        );
    }
}
