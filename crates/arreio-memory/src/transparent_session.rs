//! TransparentSessionManager — gerenciamento automático de sessões para usuários leigos.
//!
//! O usuário nunca precisa criar, resumir, listar ou gerenciar sessões.
//! O sistema faz tudo automaticamente:
//!   • Resume sessão ativa recente (< 30 min)
//!   • Cria nova quando anterior é velha ou esgotada
//!   • Fork automático quando contexto explode
//!   • Auto-suspende por inatividade
//!   • Gera título amigável automaticamente
//!
//! Princípio: o sistema decide, o usuário conversa.

use anyhow::{Context, Result};
use arreio_kernel::Blackboard;

use crate::session::{ChatMessage, ChatRole, Session, SessionManager, SessionMode};

/// Tempo de inatividade para auto-suspender (segundos).
const AUTO_SUSPEND_SECONDS: u64 = 30 * 60; // 30 minutos

/// Tempo para considerar uma sessão "recente" (segundos).
const RECENT_SESSION_SECONDS: u64 = 30 * 60; // 30 minutos

/// Gerenciador transparente de sessões conversacionais.
///
/// Esconde toda a complexidade de CRUD de sessões do usuário leigo.
/// O usuário simplesmente conversa; o sistema cuida do resto.
pub struct TransparentSessionManager {
    inner: SessionManager,
    blackboard: Blackboard,
}

impl TransparentSessionManager {
    /// Cria um novo gerenciador transparente.
    pub fn new(blackboard: Blackboard) -> Self {
        let inner = SessionManager::new(blackboard.clone());
        Self { inner, blackboard }
    }

    // ── Entry Point Principal ────────────────────────────────────────────────

    /// Obtém ou cria a sessão ativa para o usuário.
    ///
    /// Lógica:
    /// 1. Verifica se há uma sessão marcada como `session::active`
    /// 2. Se sim e for recente → resume e retorna
    /// 3. Se sim mas for velha → suspende a antiga, cria nova
    /// 4. Se não → cria nova
    pub fn get_or_create(&self, source: &str, model: &str) -> Result<ActiveSession> {
        let now = now_epoch_secs();

        // 1. Verifica sessão ativa persistida
        if let Some(active) = self.blackboard.get_tuple("session", "active") {
            if let Ok(meta) = serde_json::from_value::<ActiveSessionMeta>(active) {
                // Verifica se a sessão ainda existe
                if let Ok(Some(session)) = self.inner.get(&meta.session_id) {
                    let inactive_time = now.saturating_sub(session.updated_at);

                    if inactive_time < RECENT_SESSION_SECONDS && !session.suspended {
                        // Sessão recente e ativa → resume
                        let resumed = self.inner.resume(&meta.session_id)?;
                        self.update_active_meta(&resumed.id, now)?;
                        return Ok(ActiveSession {
                            session: resumed,
                            is_new: false,
                            resumed_from: None,
                        });
                    }

                    // Sessão velha → suspende
                    let _ = self.inner.suspend(&meta.session_id);
                }
            }
        }

        // 2. Cria nova sessão
        let new_session = self
            .inner
            .create(source, model, SessionMode::Conversational)?;
        self.update_active_meta(&new_session.id, now)?;

        Ok(ActiveSession {
            session: new_session,
            is_new: true,
            resumed_from: None,
        })
    }

    /// Registra atividade na sessão atual (atualiza timestamp).
    pub fn touch(&self, session_id: &str) -> Result<()> {
        let now = now_epoch_secs();
        self.update_active_meta(session_id, now)?;

        // Atualiza updated_at da sessão
        if let Some(mut session) = self.inner.get(session_id)? {
            session.updated_at = now;
            self.inner.update(&session)?;
        }

        Ok(())
    }

    // ── Auto-Fork ────────────────────────────────────────────────────────────

    /// Verifica se a sessão precisa de fork automático (budget exaurido).
    /// Se sim, cria sessão filha e retorna a nova.
    pub fn auto_fork_if_needed(&self, session_id: &str) -> Result<Option<Session>> {
        let budget = self.inner.get_budget(session_id)?;

        if budget.used_tokens >= budget.max_tokens {
            // Gera summary automático a partir das últimas mensagens
            let summary = self.generate_session_summary(session_id)?;

            // Cria fork
            let child = self.inner.fork(session_id, Some(&summary))?;

            // Adiciona mensagem de contexto na sessão filha
            let context_msg = format!(
                "[Contexto anterior resumido] {}\n\nContinuamos de onde paramos.",
                summary
            );
            self.inner.append_message(
                &child.id,
                ChatRole::System,
                &context_msg,
                None,
                None,
                context_msg.len() / 4,
            )?;

            // Atualiza active para apontar para a filha
            self.update_active_meta(&child.id, now_epoch_secs())?;

            return Ok(Some(child));
        }

        Ok(None)
    }

    // ── Auto-Suspend ─────────────────────────────────────────────────────────

    /// Suspende sessões inativas há mais de `AUTO_SUSPEND_SECONDS`.
    /// Retorna o número de sessões suspensas.
    pub fn suspend_stale(&self) -> Result<usize> {
        let now = now_epoch_secs();
        let sessions = self.inner.list()?;
        let mut suspended = 0;

        for session in sessions {
            if session.suspended {
                continue;
            }
            let inactive = now.saturating_sub(session.updated_at);
            if inactive >= AUTO_SUSPEND_SECONDS {
                self.inner.suspend(&session.id)?;
                suspended += 1;
            }
        }

        Ok(suspended)
    }

    // ── Título Automático ────────────────────────────────────────────────────

    /// Gera um título amigável a partir da primeira mensagem do usuário.
    pub fn auto_title(&self, first_user_message: &str) -> String {
        let words: Vec<&str> = first_user_message
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .take(6)
            .collect();

        if words.is_empty() {
            return "Nova conversa".to_string();
        }

        let title = words.join(" ");
        let mut title = title;
        // Capitaliza primeira letra
        if let Some(first) = title.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        title
    }

    /// Atualiza o título da sessão se ainda não tiver um.
    pub fn set_title_if_empty(&self, session_id: &str, title: &str) -> Result<()> {
        if let Some(mut session) = self.inner.get(session_id)? {
            if session.title.is_none() {
                session.title = Some(title.to_string());
                self.inner.update(&session)?;
            }
        }
        Ok(())
    }

    // ── Cross-Session Memory ─────────────────────────────────────────────────

    /// Adiciona uma mensagem à sessão (proxy para SessionManager).
    pub fn append_message(
        &self,
        session_id: &str,
        role: ChatRole,
        content: &str,
        tool_calls: Option<Vec<crate::context_compressor::ToolCallRef>>,
        tool_call_id: Option<String>,
        tokens: usize,
    ) -> Result<ChatMessage> {
        self.inner
            .append_message(session_id, role, content, tool_calls, tool_call_id, tokens)
    }

    /// Lista mensagens de uma sessão (proxy para SessionManager).
    pub fn list_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        self.inner.list_messages(session_id)
    }

    /// Obtém uma sessão pelo ID (proxy para SessionManager).
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        self.inner.get(session_id)
    }

    /// Cria uma nova sessão (proxy para SessionManager).
    pub fn create_session(&self, source: &str, model: &str, mode: SessionMode) -> Result<Session> {
        self.inner.create(source, model, mode)
    }

    /// Lista todas as sessões (proxy para SessionManager).
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        self.inner.list()
    }

    /// Recupera o contexto da sessão anterior (parent) se existir.
    /// Usado para hidratação de contexto ao retomar.
    pub fn get_parent_context(&self, session_id: &str) -> Result<Option<String>> {
        let session = self
            .inner
            .get(session_id)?
            .with_context(|| format!("sessão {} não encontrada", session_id))?;

        if let Some(parent_id) = session.parent_id {
            let msgs = self.inner.list_messages(&parent_id)?;
            if msgs.is_empty() {
                return Ok(None);
            }
            // Pega as últimas 3 mensagens como contexto
            let recent: Vec<String> = msgs
                .iter()
                .rev()
                .take(3)
                .map(|m| {
                    format!(
                        "[{}] {}",
                        m.role,
                        m.content.chars().take(100).collect::<String>()
                    )
                })
                .collect();
            let context = recent.into_iter().rev().collect::<Vec<_>>().join("\n");
            return Ok(Some(context));
        }

        Ok(None)
    }

    // ── Listagem Amigável ────────────────────────────────────────────────────

    /// Lista sessões para exibição ao usuário (títulos amigáveis, não IDs).
    pub fn list_friendly(&self, limit: usize) -> Result<Vec<FriendlySession>> {
        let sessions = self.inner.list()?;
        let mut result = Vec::new();

        for session in sessions.into_iter().take(limit) {
            let msg_count = self.inner.message_count(&session.id).unwrap_or(0);
            result.push(FriendlySession {
                id: session.id.clone(),
                title: session
                    .title
                    .clone()
                    .unwrap_or_else(|| "Conversa sem título".to_string()),
                updated_at: session.updated_at,
                message_count: msg_count,
                is_active: !session.suspended,
            });
        }

        Ok(result)
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn update_active_meta(&self, session_id: &str, timestamp: u64) -> Result<()> {
        let meta = ActiveSessionMeta {
            session_id: session_id.to_string(),
            last_activity: timestamp,
        };
        self.blackboard
            .put_tuple("session", "active", serde_json::to_value(&meta)?)
    }

    fn generate_session_summary(&self, session_id: &str) -> Result<String> {
        let msgs = self.inner.list_messages(session_id)?;
        if msgs.is_empty() {
            return Ok("Conversa anterior".to_string());
        }

        // Pega as primeiras 2 mensagens do usuário como "tema"
        let user_msgs: Vec<&str> = msgs
            .iter()
            .filter(|m| m.role == ChatRole::User)
            .take(2)
            .map(|m| m.content.as_str())
            .collect();

        if user_msgs.is_empty() {
            return Ok("Conversa anterior".to_string());
        }

        let theme = user_msgs.join("; ").chars().take(80).collect::<String>();
        Ok(format!("Tema: {}", theme))
    }
}

// ── Estruturas auxiliares ───────────────────────────────────────────────────

/// Metadados da sessão ativa persistida no Blackboard.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ActiveSessionMeta {
    pub session_id: String,
    pub last_activity: u64,
}

/// Sessão ativa retornada ao caller.
#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub session: Session,
    /// True se a sessão foi criada agora (não retomada).
    pub is_new: bool,
    /// Se retomada de uma sessão anterior (fork), contém o ID do parent.
    pub resumed_from: Option<String>,
}

/// Representação amigável de uma sessão para exibição ao usuário.
#[derive(Debug, Clone)]
pub struct FriendlySession {
    pub id: String,
    pub title: String,
    pub updated_at: u64,
    pub message_count: usize,
    pub is_active: bool,
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

    fn temp_manager() -> TransparentSessionManager {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        TransparentSessionManager::new(bb)
    }

    #[test]
    fn get_or_create_cria_nova_sessao() {
        let mgr = temp_manager();
        let active = mgr.get_or_create("cli", "gemma4").unwrap();
        assert!(active.is_new);
        assert!(!active.session.id.is_empty());
    }

    #[test]
    fn get_or_create_retoma_sessao_recente() {
        let mgr = temp_manager();
        let first = mgr.get_or_create("cli", "gemma4").unwrap();
        let second = mgr.get_or_create("cli", "gemma4").unwrap();
        assert!(!second.is_new);
        assert_eq!(second.session.id, first.session.id);
    }

    #[test]
    fn auto_title_gera_titulo_amigavel() {
        let mgr = temp_manager();
        let title = mgr.auto_title("Preciso de uma planilha de estoque");
        // Palavras com <= 2 chars são filtradas ("de" é removido)
        assert_eq!(title, "Preciso uma planilha estoque");
    }

    #[test]
    fn auto_title_mensagem_curta() {
        let mgr = temp_manager();
        let title = mgr.auto_title("Oi");
        assert_eq!(title, "Nova conversa");
    }

    #[test]
    fn set_title_if_empty_funciona() {
        let mgr = temp_manager();
        let active = mgr.get_or_create("cli", "gemma4").unwrap();
        assert!(active.session.title.is_none());

        mgr.set_title_if_empty(&active.session.id, "Minha conversa")
            .unwrap();
        let updated = mgr.inner.get(&active.session.id).unwrap().unwrap();
        assert_eq!(updated.title, Some("Minha conversa".to_string()));
    }

    #[test]
    fn suspend_stale_nao_suspense_sessao_recente() {
        let mgr = temp_manager();
        let active = mgr.get_or_create("cli", "gemma4").unwrap();

        // Sessão recém-criada não deve ser suspensa
        let suspended = mgr.suspend_stale().unwrap();
        assert_eq!(suspended, 0, "sessão recente não deve ser suspensa");

        // Verifica que a sessão continua ativa
        let s = mgr.inner.get(&active.session.id).unwrap().unwrap();
        assert!(!s.suspended, "sessão recente deve continuar ativa");
    }

    #[test]
    fn list_friendly_retorna_titulos() {
        let mgr = temp_manager();
        let active = mgr.get_or_create("cli", "gemma4").unwrap();
        mgr.set_title_if_empty(&active.session.id, "Conversa teste")
            .unwrap();

        let list = mgr.list_friendly(10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Conversa teste");
        assert!(list[0].is_active);
    }

    #[test]
    fn touch_atualiza_atividade() {
        let mgr = temp_manager();
        let active = mgr.get_or_create("cli", "gemma4").unwrap();

        mgr.touch(&active.session.id).unwrap();

        let meta = mgr.blackboard.get_tuple("session", "active").unwrap();
        let parsed: ActiveSessionMeta = serde_json::from_value(meta).unwrap();
        assert_eq!(parsed.session_id, active.session.id);
        assert!(parsed.last_activity > 0);
    }
}
