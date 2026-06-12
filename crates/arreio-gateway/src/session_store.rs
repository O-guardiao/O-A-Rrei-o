use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Sessão persistente para auto-resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub session_key: String,
    pub platform: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub user_id: String,
    pub created_at: u64,
    pub last_message_at: u64,
    pub resume_pending: bool,
    pub suspended: bool,
    pub was_auto_reset: bool,
}

/// Store de sessões persistido no Blackboard.
pub struct SessionStore {
    blackboard: Blackboard,
}

impl SessionStore {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    fn sessions_key() -> &'static str {
        "gateway:sessions"
    }

    fn load(&self) -> HashMap<String, Session> {
        self.blackboard
            .get_tuple("gateway", Self::sessions_key())
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    fn save(&self, sessions: &HashMap<String, Session>) -> Result<()> {
        let value = serde_json::to_value(sessions)?;
        self.blackboard
            .put_tuple("gateway", Self::sessions_key(), value)
    }

    /// Cria ou atualiza uma sessão.
    pub fn upsert(&self, session: Session) -> Result<()> {
        let mut sessions = self.load();
        sessions.insert(session.session_id.clone(), session);
        self.save(&sessions)
    }

    /// Obtém sessão por ID.
    pub fn get(&self, session_id: &str) -> Option<Session> {
        self.load().get(session_id).cloned()
    }

    /// Obtém sessão por session_key.
    pub fn get_by_key(&self, session_key: &str) -> Option<Session> {
        self.load()
            .values()
            .find(|s| s.session_key == session_key)
            .cloned()
    }

    /// Marca sessões ativas nos últimos `seconds` como resume_pending.
    pub fn mark_resume_pending(&self, seconds: u64) -> Result<Vec<Session>> {
        let now = now_epoch_secs();
        let cutoff = now - seconds;
        let mut sessions = self.load();
        let mut pending = Vec::new();
        for session in sessions.values_mut() {
            if !session.suspended && session.last_message_at >= cutoff {
                session.resume_pending = true;
                pending.push(session.clone());
            }
        }
        self.save(&sessions)?;
        Ok(pending)
    }

    /// Limpa flag resume_pending de uma sessão.
    pub fn clear_resume_pending(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.load();
        if let Some(session) = sessions.get_mut(session_id) {
            session.resume_pending = false;
        }
        self.save(&sessions)
    }

    /// Remove sessões stale (sem mensagem há `days` dias).
    pub fn prune_stale(&self, days: u64) -> Result<usize> {
        let now = now_epoch_secs();
        let cutoff = now - (days * 86400);
        let mut sessions = self.load();
        let before = sessions.len();
        sessions.retain(|_, s| s.last_message_at >= cutoff);
        let after = sessions.len();
        self.save(&sessions)?;
        Ok(before - after)
    }

    /// Lista todas as sessões.
    pub fn list(&self) -> Vec<Session> {
        self.load().values().cloned().collect()
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
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_store() -> SessionStore {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        SessionStore::new(bb)
    }

    fn make_session(id: &str, last_msg: u64) -> Session {
        Session {
            session_id: id.to_string(),
            session_key: format!("key-{}", id),
            platform: "test".to_string(),
            chat_id: "chat1".to_string(),
            thread_id: None,
            user_id: "user1".to_string(),
            created_at: last_msg,
            last_message_at: last_msg,
            resume_pending: false,
            suspended: false,
            was_auto_reset: false,
        }
    }

    #[test]
    fn upsert_and_get() {
        let store = temp_store();
        let session = make_session("s1", 1000);
        store.upsert(session.clone()).unwrap();
        let retrieved = store.get("s1").unwrap();
        assert_eq!(retrieved.session_key, "key-s1");
    }

    #[test]
    fn get_by_key() {
        let store = temp_store();
        let session = make_session("s1", 1000);
        store.upsert(session).unwrap();
        let found = store.get_by_key("key-s1").unwrap();
        assert_eq!(found.session_id, "s1");
    }

    #[test]
    fn mark_resume_pending() {
        let store = temp_store();
        let now = now_epoch_secs();
        store.upsert(make_session("active", now)).unwrap();
        store.upsert(make_session("old", now - 300)).unwrap();

        let pending = store.mark_resume_pending(120).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_id, "active");
    }

    #[test]
    fn clear_resume_pending() {
        let store = temp_store();
        let now = now_epoch_secs();
        let mut session = make_session("s1", now);
        session.resume_pending = true;
        store.upsert(session).unwrap();

        store.clear_resume_pending("s1").unwrap();
        let retrieved = store.get("s1").unwrap();
        assert!(!retrieved.resume_pending);
    }

    #[test]
    fn prune_stale_sessions() {
        let store = temp_store();
        let now = now_epoch_secs();
        store.upsert(make_session("fresh", now)).unwrap();
        store
            .upsert(make_session("stale", now - 86400 * 10))
            .unwrap();

        let pruned = store.prune_stale(7).unwrap();
        assert_eq!(pruned, 1);
        assert!(store.get("stale").is_none());
        assert!(store.get("fresh").is_some());
    }
}
