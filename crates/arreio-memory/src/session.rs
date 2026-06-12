//! SessionManager — persistência de sessões conversacionais no Blackboard.
//!
//! Traduzido do Hermes Agent (SQLite sessions) e Agent Memory (MemoryBox)
//! para a arquitetura O Arreio: tudo é tupla JSON no Blackboard (cat::key).
//!
//! Convenção de tuplas:
//!   session::<uuid>              → metadados da sessão
//!   session::<uuid>::msg::<seq>  → mensagens individuais
//!   session::<uuid>::budget      → orçamento de tokens e threshold

use anyhow::{Context, Result};
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

// Reutiliza ToolCallRef do context_compressor para evitar duplicação.
pub use crate::context_compressor::ToolCallRef;

/// Papel de uma mensagem no chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for ChatRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChatRole::System => write!(f, "system"),
            ChatRole::User => write!(f, "user"),
            ChatRole::Assistant => write!(f, "assistant"),
            ChatRole::Tool => write!(f, "tool"),
        }
    }
}

/// Uma mensagem dentro de uma sessão conversacional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub seq: usize,
    pub role: ChatRole,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCallRef>>,
    pub tool_call_id: Option<String>,
    pub timestamp: u64,
    pub tokens: usize,
}

/// Metadados de uma sessão conversacional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub source: String, // "cli", "gateway", "telegram", etc.
    pub model: String,
    pub title: Option<String>,
    pub token_in: usize,
    pub token_out: usize,
    pub parent_id: Option<String>, // sessão anterior quando há fork
    pub suspended: bool,
    pub mode: SessionMode,
}

/// Modo de operação da sessão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    Conversational,
    Task,
}

impl std::fmt::Display for SessionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMode::Conversational => write!(f, "conversational"),
            SessionMode::Task => write!(f, "task"),
        }
    }
}

/// Orçamento de contexto de uma sessão.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContextBudget {
    pub max_tokens: usize,
    pub used_tokens: usize,
    pub threshold_percent: f32,
    /// Contador de falhas consecutivas de auto-compact (GAP-004 — Circuit Breaker).
    /// Threshold: 3 falhas → circuit breaker abre, loop encerrado.
    pub consecutive_autocompact_failures: u8,
}

impl Default for SessionContextBudget {
    fn default() -> Self {
        Self {
            max_tokens: 32768,
            used_tokens: 0,
            threshold_percent: 0.75,
            consecutive_autocompact_failures: 0,
        }
    }
}

impl SessionContextBudget {
    pub fn threshold_tokens(&self) -> usize {
        (self.max_tokens as f32 * self.threshold_percent) as usize
    }

    pub fn is_over_threshold(&self) -> bool {
        self.used_tokens > self.threshold_tokens()
    }

    pub fn remaining(&self) -> usize {
        self.max_tokens.saturating_sub(self.used_tokens)
    }
}

/// Gerenciador de sessões conversacionais persistidas no Blackboard.
pub struct SessionManager {
    blackboard: Blackboard,
}

impl SessionManager {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    // ── CRUD de Sessão ───────────────────────────────────────────────────────

    /// Cria uma nova sessão conversacional.
    pub fn create(&self, source: &str, model: &str, mode: SessionMode) -> Result<Session> {
        let id = format!("sess_{}", uuid::Uuid::new_v4());
        let now = now_epoch_secs();
        let session = Session {
            id: id.clone(),
            created_at: now,
            updated_at: now,
            source: source.into(),
            model: model.into(),
            title: None,
            token_in: 0,
            token_out: 0,
            parent_id: None,
            suspended: false,
            mode,
        };
        self.put_session(&session)?;
        // Inicializa budget padrão
        self.put_budget(&id, &SessionContextBudget::default())?;
        Ok(session)
    }

    /// Obtém uma sessão pelo ID.
    pub fn get(&self, session_id: &str) -> Result<Option<Session>> {
        match self.blackboard.get_tuple("session", session_id) {
            Some(v) => {
                let session: Session = serde_json::from_value(v)
                    .with_context(|| format!("falha ao desserializar sessão {}", session_id))?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    /// Atualiza metadados de uma sessão.
    pub fn update(&self, session: &Session) -> Result<()> {
        let mut s = session.clone();
        s.updated_at = now_epoch_secs();
        self.put_session(&s)
    }

    /// Remove uma sessão e todas as suas mensagens.
    pub fn delete(&self, session_id: &str) -> Result<()> {
        // Remove metadados
        self.blackboard.delete_tuple("session", session_id)?;
        // Remove budget
        self.blackboard
            .delete_tuple("session", &format!("{}::budget", session_id))?;
        // Remove mensagens (procura todas as chaves que começam com session_id::msg::)
        let all = self
            .blackboard
            .search_tuples("session", &format!("{}::msg::", session_id));
        for (key, _) in all {
            self.blackboard.delete_tuple("session", &key)?;
        }
        Ok(())
    }

    /// Lista todas as sessões existentes.
    pub fn list(&self) -> Result<Vec<Session>> {
        let all = self.blackboard.search_tuples("session", "");
        let mut sessions = Vec::new();
        for (key, value) in all {
            // Só pega chaves que são IDs diretos de sessão (não mensagens nem budget)
            if key.contains("::") {
                continue;
            }
            if let Ok(session) = serde_json::from_value::<Session>(value) {
                sessions.push(session);
            }
        }
        // Ordena por updated_at decrescente
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    /// Lista sessões ativas nos últimos `seconds` segundos.
    pub fn list_active(&self, seconds: u64) -> Result<Vec<Session>> {
        let now = now_epoch_secs();
        let cutoff = now - seconds;
        Ok(self
            .list()?
            .into_iter()
            .filter(|s| !s.suspended && s.updated_at >= cutoff)
            .collect())
    }

    // ── Mensagens ────────────────────────────────────────────────────────────

    /// Adiciona uma mensagem à sessão.
    pub fn append_message(
        &self,
        session_id: &str,
        role: ChatRole,
        content: &str,
        tool_calls: Option<Vec<ToolCallRef>>,
        tool_call_id: Option<String>,
        tokens: usize,
    ) -> Result<ChatMessage> {
        let seq = self.next_seq(session_id)?;
        let msg = ChatMessage {
            seq,
            role,
            content: content.into(),
            tool_calls,
            tool_call_id,
            timestamp: now_epoch_secs(),
            tokens,
        };
        let key = format!("{}::msg::{}", session_id, seq);
        self.blackboard
            .put_tuple("session", &key, serde_json::to_value(&msg)?)?;

        // Atualiza contadores da sessão
        if let Some(mut session) = self.get(session_id)? {
            match role {
                ChatRole::User => session.token_in += tokens,
                ChatRole::Assistant => session.token_out += tokens,
                _ => {}
            }
            self.update(&session)?;
        }

        Ok(msg)
    }

    /// Obtém uma mensagem específica.
    pub fn get_message(&self, session_id: &str, seq: usize) -> Result<Option<ChatMessage>> {
        let key = format!("{}::msg::{}", session_id, seq);
        match self.blackboard.get_tuple("session", &key) {
            Some(v) => {
                let msg: ChatMessage = serde_json::from_value(v).with_context(|| {
                    format!("falha ao desserializar msg {}:{}", session_id, seq)
                })?;
                Ok(Some(msg))
            }
            None => Ok(None),
        }
    }

    /// Lista todas as mensagens de uma sessão, ordenadas por seq.
    pub fn list_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let prefix = format!("{}::msg::", session_id);
        let all = self.blackboard.search_tuples("session", &prefix);
        let mut messages: Vec<ChatMessage> = all
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect();
        messages.sort_by_key(|m| m.seq);
        Ok(messages)
    }

    /// Conta mensagens de uma sessão.
    pub fn message_count(&self, session_id: &str) -> Result<usize> {
        self.list_messages(session_id).map(|v| v.len())
    }

    /// Remove todas as mensagens de uma sessão (mantém metadados e budget).
    pub fn clear_messages(&self, session_id: &str) -> Result<()> {
        let prefix = format!("{}::msg::", session_id);
        let all = self.blackboard.search_tuples("session", &prefix);
        for (key, _) in all {
            self.blackboard.delete_tuple("session", &key)?;
        }
        Ok(())
    }

    /// Substitui todas as mensagens de uma sessão por um novo conjunto.
    /// Usado após compressão de contexto.
    pub fn replace_messages(&self, session_id: &str, messages: &[ChatMessage]) -> Result<()> {
        self.clear_messages(session_id)?;
        for (i, msg) in messages.iter().enumerate() {
            let key = format!("{}::msg::{}", session_id, i);
            let mut new_msg = msg.clone();
            new_msg.seq = i;
            self.blackboard
                .put_tuple("session", &key, serde_json::to_value(&new_msg)?)?;
        }
        Ok(())
    }

    // ── Budget ───────────────────────────────────────────────────────────────

    /// Obtém o budget de contexto de uma sessão.
    pub fn get_budget(&self, session_id: &str) -> Result<SessionContextBudget> {
        let key = format!("{}::budget", session_id);
        match self.blackboard.get_tuple("session", &key) {
            Some(v) => {
                let budget: SessionContextBudget = serde_json::from_value(v)
                    .with_context(|| format!("falha ao desserializar budget de {}", session_id))?;
                Ok(budget)
            }
            None => Ok(SessionContextBudget::default()),
        }
    }

    /// Atualiza o budget de contexto.
    pub fn put_budget(&self, session_id: &str, budget: &SessionContextBudget) -> Result<()> {
        let key = format!("{}::budget", session_id);
        self.blackboard
            .put_tuple("session", &key, serde_json::to_value(budget)?)
    }

    /// Atualiza tokens usados no budget.
    pub fn add_tokens(&self, session_id: &str, tokens: usize) -> Result<()> {
        let mut budget = self.get_budget(session_id)?;
        budget.used_tokens += tokens;
        self.put_budget(session_id, &budget)
    }

    // ── Fork / Resume ────────────────────────────────────────────────────────

    /// Cria uma nova sessão "filha" apontando para a sessão atual como pai.
    /// Usado quando o contexto explode e precisa ser bifurcado.
    pub fn fork(&self, session_id: &str, title: Option<&str>) -> Result<Session> {
        let parent = self
            .get(session_id)?
            .with_context(|| format!("sessão {} não encontrada", session_id))?;

        let child = self.create(&parent.source, &parent.model, parent.mode)?;
        let mut child_meta = child.clone();
        child_meta.parent_id = Some(parent.id.clone());
        child_meta.title = title.map(String::from);
        self.put_session(&child_meta)?;
        Ok(child_meta)
    }

    /// Resume uma sessão existente (apenas marca como não suspensa).
    pub fn resume(&self, session_id: &str) -> Result<Session> {
        let mut session = self
            .get(session_id)?
            .with_context(|| format!("sessão {} não encontrada", session_id))?;
        session.suspended = false;
        self.update(&session)?;
        Ok(session)
    }

    /// Suspende uma sessão.
    pub fn suspend(&self, session_id: &str) -> Result<()> {
        if let Some(mut session) = self.get(session_id)? {
            session.suspended = true;
            self.update(&session)?;
        }
        Ok(())
    }

    /// Remove sessões antigas (prune).
    pub fn prune_stale(&self, days: u64) -> Result<usize> {
        let now = now_epoch_secs();
        let cutoff = now - (days * 86400);
        let sessions = self.list()?;
        let mut removed = 0;
        for s in sessions {
            if s.updated_at < cutoff {
                self.delete(&s.id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    // ── Helpers privados ─────────────────────────────────────────────────────

    fn put_session(&self, session: &Session) -> Result<()> {
        self.blackboard
            .put_tuple("session", &session.id, serde_json::to_value(session)?)
    }

    fn next_seq(&self, session_id: &str) -> Result<usize> {
        let count = self.message_count(session_id)?;
        Ok(count)
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
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_manager() -> SessionManager {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        SessionManager::new(bb)
    }

    #[test]
    fn cria_sessao_e_recupera() {
        let mgr = temp_manager();
        let s = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        assert!(!s.id.is_empty());
        assert_eq!(s.source, "cli");
        assert_eq!(s.model, "gemma4");
        assert_eq!(s.mode, SessionMode::Conversational);

        let found = mgr.get(&s.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, s.id);
    }

    #[test]
    fn adiciona_mensagens_e_lista() {
        let mgr = temp_manager();
        let s = mgr
            .create("gateway", "gemma4", SessionMode::Conversational)
            .unwrap();

        mgr.append_message(&s.id, ChatRole::User, "Olá", None, None, 2)
            .unwrap();
        mgr.append_message(&s.id, ChatRole::Assistant, "Oi!", None, None, 2)
            .unwrap();
        mgr.append_message(&s.id, ChatRole::User, "Como vai?", None, None, 3)
            .unwrap();

        let msgs = mgr.list_messages(&s.id).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, ChatRole::User);
        assert_eq!(msgs[1].role, ChatRole::Assistant);
        assert_eq!(msgs[2].content, "Como vai?");
    }

    #[test]
    fn budget_atualiza_com_tokens() {
        let mgr = temp_manager();
        let s = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        let b = mgr.get_budget(&s.id).unwrap();
        assert_eq!(b.used_tokens, 0);
        assert_eq!(b.max_tokens, 32768);

        mgr.add_tokens(&s.id, 150).unwrap();
        let b2 = mgr.get_budget(&s.id).unwrap();
        assert_eq!(b2.used_tokens, 150);
        assert!(!b2.is_over_threshold());
    }

    #[test]
    fn fork_cria_sessao_filha() {
        let mgr = temp_manager();
        let parent = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        let child = mgr.fork(&parent.id, Some("continuação")).unwrap();

        assert_eq!(child.parent_id, Some(parent.id.clone()));
        assert_eq!(child.title, Some("continuação".into()));
        assert_eq!(child.mode, SessionMode::Conversational);
    }

    #[test]
    fn resume_e_suspend() {
        let mgr = temp_manager();
        let s = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        assert!(!s.suspended);

        mgr.suspend(&s.id).unwrap();
        let s2 = mgr.get(&s.id).unwrap().unwrap();
        assert!(s2.suspended);

        let s3 = mgr.resume(&s.id).unwrap();
        assert!(!s3.suspended);
    }

    #[test]
    fn prune_remove_antigas() {
        let mgr = temp_manager();
        let s1 = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        // simula sessão antiga alterando updated_at diretamente no Blackboard
        let mut old = s1.clone();
        old.updated_at = 0;
        mgr.put_session(&old).unwrap();

        let s2 = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();

        let removed = mgr.prune_stale(1).unwrap();
        assert_eq!(removed, 1);
        assert!(mgr.get(&s2.id).unwrap().is_some());
        assert!(mgr.get(&s1.id).unwrap().is_none());
    }

    #[test]
    fn lista_ativas_filtra_suspensas() {
        let mgr = temp_manager();
        let s1 = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        let s2 = mgr
            .create("gateway", "gemma4", SessionMode::Conversational)
            .unwrap();
        mgr.suspend(&s2.id).unwrap();

        let active = mgr.list_active(3600).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, s1.id);
    }

    #[test]
    fn delete_remove_tudo() {
        let mgr = temp_manager();
        let s = mgr
            .create("cli", "gemma4", SessionMode::Conversational)
            .unwrap();
        mgr.append_message(&s.id, ChatRole::User, "test", None, None, 1)
            .unwrap();

        mgr.delete(&s.id).unwrap();
        assert!(mgr.get(&s.id).unwrap().is_none());
        assert_eq!(mgr.list_messages(&s.id).unwrap().len(), 0);
    }
}
