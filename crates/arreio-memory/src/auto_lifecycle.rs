//! AutoLifecycle — ciclo de vida automático de sessões conversacionais.
//!
//! Gerencia estados de sessão sem intervenção do usuário:
//!   • Active → Paused (inatividade > 30 min)
//!   • Active → Complete (usuário diz "tchau", "obrigado, isso é tudo")
//!   • Active → BudgetLimited (budget exaurido)
//!   • Paused → Active (usuário volta a interagir)
//!   • Detecta anti-loop (3 turnos sem progresso)
//!
//! Princípio: o sistema decide, o usuário conversa.

use arreio_kernel::Blackboard;

use crate::session::{ChatMessage, ChatRole, SessionManager};

/// Tempo de inatividade para auto-pausar (segundos).
const INACTIVITY_PAUSE_SECONDS: u64 = 30 * 60; // 30 minutos

/// Palavras de despedida que indicam fim de conversa.
const CLOSING_WORDS: &[&str] = &[
    "tchau",
    "adeus",
    "até logo",
    "ate logo",
    "até mais",
    "ate mais",
    "até breve",
    "ate breve",
    "até amanhã",
    "ate amanha",
    "flw",
    "falou",
    "obrigado, isso é tudo",
    "obrigada, isso é tudo",
    "obrigado isso é tudo",
    "obrigada isso é tudo",
    "isso é tudo",
    "isso e tudo",
    "não preciso mais",
    "nao preciso mais",
    "por hoje é só",
    "por hoje e so",
    "já resolvi",
    "ja resolvi",
    "problema resolvido",
    "resolvido",
    "resolvida",
    "pronto",
    "finalizado",
    "terminado",
    "concluído",
    "concluido",
    "fechado",
    "fechada",
    "valeu, tchau",
    "valeu tchau",
    "ok, tchau",
    "ok tchau",
    "bye",
    "goodbye",
    "see you",
    "later",
    "cya",
];

/// Palavras de saudação que indicam início de conversa.
const GREETING_WORDS: &[&str] = &[
    "bom dia",
    "boa tarde",
    "boa noite",
    "oi",
    "olá",
    "ola",
    "ei",
    "hello",
    "hi",
    "hey",
    "salve",
    "fala",
    "e aí",
    "eai",
];

/// Gerenciador automático de lifecycle de sessões.
pub struct AutoLifecycle {
    #[allow(dead_code)]
    session_mgr: SessionManager,
}

impl AutoLifecycle {
    /// Cria um novo gerenciador de lifecycle.
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            session_mgr: SessionManager::new(blackboard),
        }
    }

    // ── Detecção de Estado ───────────────────────────────────────────────────

    /// Analisa a última mensagem do usuário e detecta se indica fim de conversa.
    pub fn detect_complete(&self, messages: &[ChatMessage]) -> bool {
        let Some(last_user) = messages.iter().rev().find(|m| m.role == ChatRole::User) else {
            return false;
        };
        self.is_closing_message(&last_user.content)
    }

    /// Analisa a última mensagem e detecta se é saudação (início de conversa).
    pub fn detect_greeting(&self, messages: &[ChatMessage]) -> bool {
        let Some(last_user) = messages.iter().rev().find(|m| m.role == ChatRole::User) else {
            return false;
        };
        self.is_greeting_message(&last_user.content)
    }

    /// Detecta anti-loop: 3 turnos consecutivos sem progresso.
    ///
    /// "Sem progresso" significa: usuário repete a mesma pergunta ou
    /// assistente dá a mesma resposta.
    pub fn detect_loop(&self, messages: &[ChatMessage]) -> bool {
        if messages.len() < 6 {
            return false;
        }

        // Pega as últimas 6 mensagens (3 turnos)
        let recent = &messages[messages.len().saturating_sub(6)..];

        // Verifica se o usuário repetiu a mesma pergunta
        let user_msgs: Vec<&str> = recent
            .iter()
            .filter(|m| m.role == ChatRole::User)
            .map(|m| m.content.as_str())
            .collect();

        if user_msgs.len() >= 3 {
            let first = Self::normalize(&user_msgs[0]);
            let second = Self::normalize(&user_msgs[1]);
            let third = Self::normalize(&user_msgs[2]);
            if first == second && second == third {
                return true;
            }
        }

        // Verifica se o assistente deu a mesma resposta
        let assistant_msgs: Vec<&str> = recent
            .iter()
            .filter(|m| m.role == ChatRole::Assistant)
            .map(|m| m.content.as_str())
            .collect();

        if assistant_msgs.len() >= 3 {
            let first = Self::normalize(&assistant_msgs[0]);
            let second = Self::normalize(&assistant_msgs[1]);
            let third = Self::normalize(&assistant_msgs[2]);
            if first == second && second == third {
                return true;
            }
        }

        false
    }

    /// Detecta se a sessão está inativa há muito tempo.
    pub fn is_inactive(&self, last_activity: u64) -> bool {
        let now = now_epoch_secs();
        let inactive = now.saturating_sub(last_activity);
        inactive >= INACTIVITY_PAUSE_SECONDS
    }

    // ── Sugestões de Ação ────────────────────────────────────────────────────

    /// Gera uma mensagem de despedida amigável quando a conversa termina.
    pub fn farewell_message(&self) -> String {
        let messages = [
            "Foi um prazer ajudar! Sua conversa foi salva. Até a próxima! 👋",
            "Obrigado por conversar comigo! Estou aqui quando precisar. 😊",
            "Fico feliz em ter ajudado! Volte sempre. 👋",
            "Até logo! Sua conversa está salva para quando precisar continuar. ✨",
        ];
        let idx = (now_epoch_secs() % messages.len() as u64) as usize;
        messages[idx].to_string()
    }

    /// Gera uma mensagem de boas-vindas ao retomar uma sessão pausada.
    pub fn welcome_back_message(&self, session_title: Option<&str>) -> String {
        match session_title {
            Some(title) => format!(
                "De onde paramos? Você estava trabalhando em '{}'. Posso continuar ajudando?",
                title
            ),
            None => "De onde paramos? Posso continuar ajudando?".to_string(),
        }
    }

    /// Gera uma mensagem sugerindo variação quando detecta loop.
    pub fn loop_suggestion(&self) -> String {
        "Parece que estamos repetindo o mesmo ponto. Que tal reformular a pergunta ou tentar uma abordagem diferente? 🤔".to_string()
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn is_closing_message(&self, content: &str) -> bool {
        let normalized = content.to_lowercase();
        for word in CLOSING_WORDS {
            if normalized.contains(word) {
                return true;
            }
        }
        false
    }

    fn is_greeting_message(&self, content: &str) -> bool {
        let normalized = content.to_lowercase();
        for word in GREETING_WORDS {
            if normalized.starts_with(word) {
                return true;
            }
        }
        false
    }

    fn normalize(s: &str) -> String {
        s.to_lowercase()
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .collect::<Vec<_>>()
            .join(" ")
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

    fn temp_lifecycle() -> AutoLifecycle {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        AutoLifecycle::new(bb)
    }

    #[test]
    fn detecta_despedida() {
        let lc = temp_lifecycle();
        let msgs = vec![ChatMessage {
            seq: 0,
            role: ChatRole::User,
            content: "Obrigado, isso é tudo".into(),
            tool_calls: None,
            tool_call_id: None,
            timestamp: 0,
            tokens: 5,
        }];
        assert!(lc.detect_complete(&msgs));
    }

    #[test]
    fn detecta_tchau() {
        let lc = temp_lifecycle();
        let msgs = vec![ChatMessage {
            seq: 0,
            role: ChatRole::User,
            content: "Tchau!".into(),
            tool_calls: None,
            tool_call_id: None,
            timestamp: 0,
            tokens: 1,
        }];
        assert!(lc.detect_complete(&msgs));
    }

    #[test]
    fn nao_detecta_despedida_em_conversa() {
        let lc = temp_lifecycle();
        let msgs = vec![ChatMessage {
            seq: 0,
            role: ChatRole::User,
            content: "Como faço para criar um arquivo?".into(),
            tool_calls: None,
            tool_call_id: None,
            timestamp: 0,
            tokens: 8,
        }];
        assert!(!lc.detect_complete(&msgs));
    }

    #[test]
    fn detecta_saudacao() {
        let lc = temp_lifecycle();
        let msgs = vec![ChatMessage {
            seq: 0,
            role: ChatRole::User,
            content: "Bom dia!".into(),
            tool_calls: None,
            tool_call_id: None,
            timestamp: 0,
            tokens: 2,
        }];
        assert!(lc.detect_greeting(&msgs));
    }

    #[test]
    fn detecta_loop_usuario() {
        let lc = temp_lifecycle();
        let msgs = vec![
            ChatMessage {
                seq: 0,
                role: ChatRole::User,
                content: "Como criar uma planilha?".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 5,
            },
            ChatMessage {
                seq: 1,
                role: ChatRole::Assistant,
                content: "Você pode usar Excel.".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 4,
            },
            ChatMessage {
                seq: 2,
                role: ChatRole::User,
                content: "Como criar uma planilha?".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 5,
            },
            ChatMessage {
                seq: 3,
                role: ChatRole::Assistant,
                content: "Você pode usar Excel.".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 4,
            },
            ChatMessage {
                seq: 4,
                role: ChatRole::User,
                content: "Como criar uma planilha?".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 5,
            },
            ChatMessage {
                seq: 5,
                role: ChatRole::Assistant,
                content: "Você pode usar Excel.".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 4,
            },
        ];
        assert!(lc.detect_loop(&msgs));
    }

    #[test]
    fn nao_detecta_loop_curto() {
        let lc = temp_lifecycle();
        let msgs = vec![
            ChatMessage {
                seq: 0,
                role: ChatRole::User,
                content: "Como criar?".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 3,
            },
            ChatMessage {
                seq: 1,
                role: ChatRole::Assistant,
                content: "Use Excel.".into(),
                tool_calls: None,
                tool_call_id: None,
                timestamp: 0,
                tokens: 2,
            },
        ];
        assert!(!lc.detect_loop(&msgs));
    }

    #[test]
    fn inatividade_detectada() {
        let lc = temp_lifecycle();
        let very_old = now_epoch_secs() - (31 * 60); // 31 minutos atrás
        assert!(lc.is_inactive(very_old));
    }

    #[test]
    fn inatividade_recente_nao_detectada() {
        let lc = temp_lifecycle();
        let recent = now_epoch_secs() - (5 * 60); // 5 minutos atrás
        assert!(!lc.is_inactive(recent));
    }

    #[test]
    fn farewell_message_nao_vazia() {
        let lc = temp_lifecycle();
        let msg = lc.farewell_message();
        assert!(!msg.is_empty());
        assert!(msg.contains("👋") || msg.contains("😊") || msg.contains("✨"));
    }

    #[test]
    fn welcome_back_com_titulo() {
        let lc = temp_lifecycle();
        let msg = lc.welcome_back_message(Some("planilha de estoque"));
        assert!(msg.contains("planilha de estoque"));
    }

    #[test]
    fn welcome_back_sem_titulo() {
        let lc = temp_lifecycle();
        let msg = lc.welcome_back_message(None);
        assert_eq!(msg, "De onde paramos? Posso continuar ajudando?");
    }
}
