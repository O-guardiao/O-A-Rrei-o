//! HelpContextual — ajuda contextual e sugestões naturais.
//!
//! Em vez de listar comandos, sugere ações baseadas no contexto atual:
//!   • Intent da última mensagem do usuário
//!   • Perfil do negócio (se existe)
//!   • Estado atual (conversando, task em progresso, etc.)
//!
//! Princípio: o sistema decide, o usuário conversa.

use arreio_kernel::Blackboard;

use crate::intent_classifier::{IntentClassifier, UserIntent};
use crate::onboarding::OnboardingWizard;

/// Gerador de sugestões contextuais.
pub struct HelpContextual {
    blackboard: Blackboard,
    classifier: IntentClassifier,
}

impl HelpContextual {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            classifier: IntentClassifier::new(),
        }
    }

    /// Gera sugestões baseadas na última mensagem do usuário.
    ///
    /// Retorna 1-3 sugestões naturais (não comandos).
    pub fn suggest(&self, last_user_message: &str) -> Vec<String> {
        let intent = self.classifier.classify(last_user_message);
        let profile = OnboardingWizard::new(self.blackboard.clone()).get_profile_or_default();

        let mut suggestions: Vec<String> = Vec::new();

        // Sugestões baseadas na intent
        match intent.intent {
            UserIntent::Task => {
                suggestions.push("Quer que eu faça mais alguma coisa com isso?".into());
                suggestions.push("Posso criar um arquivo com isso?".into());
                suggestions.push("Quer que eu explique como funciona?".into());
            }
            UserIntent::Conversational => {
                suggestions.push("Quer que eu transforme isso em uma tarefa?".into());
                suggestions.push("Posso criar um exemplo prático?".into());
                suggestions.push("Quer que eu explique de outro jeito?".into());
            }
            UserIntent::Hybrid => {
                suggestions.push("Quer que eu execute isso agora?".into());
                suggestions.push("Posso criar um arquivo com isso?".into());
                suggestions.push("Quer que eu detalhe mais?".into());
            }
        }

        // Sugestões baseadas no perfil
        let activity_lower = profile.activity.to_lowercase();
        let goal_lower = profile.goal.to_lowercase();

        if activity_lower.contains("venda")
            || activity_lower.contains("loja")
            || activity_lower.contains("comércio")
        {
            suggestions.push("Posso criar uma planilha de estoque?".into());
            suggestions.push("Quer um controle de vendas?".into());
        } else if activity_lower.contains("consultoria") || activity_lower.contains("escritório") {
            suggestions.push("Posso gerar um relatório?".into());
            suggestions.push("Quer uma proposta comercial?".into());
        } else if activity_lower.contains("produção") || activity_lower.contains("fábrica") {
            suggestions.push("Posso criar um controle de produção?".into());
            suggestions.push("Quer uma planilha de qualidade?".into());
        }

        if goal_lower.contains("planilha")
            || goal_lower.contains("excel")
            || goal_lower.contains("tabela")
        {
            suggestions.push("Posso criar outra planilha?".into());
            suggestions.push("Quer um modelo pronto?".into());
        }

        if goal_lower.contains("controle") || goal_lower.contains("gestão") {
            suggestions.push("Posso criar um controle automatizado?".into());
            suggestions.push("Quer um dashboard simples?".into());
        }

        // Remove duplicatas e limita a 3
        let mut unique = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for s in suggestions {
            if seen.insert(s.clone()) && unique.len() < 3 {
                unique.push(s);
            }
        }

        unique
    }

    /// Gera mensagem de ajuda contextual (comando `?`).
    pub fn help_message(&self, last_user_message: Option<&str>) -> String {
        let mut msg = String::from("💡 Você pode:\n");

        let suggestions = match last_user_message {
            Some(m) => self.suggest(m),
            None => vec![
                "Fazer uma pergunta".into(),
                "Pedir para criar algo".into(),
                "Solicitar uma planilha".into(),
            ],
        };

        for (i, s) in suggestions.iter().enumerate() {
            msg.push_str(&format!("  {}. {}\n", i + 1, s));
        }

        msg.push_str("\nComandos opcionais: /help, /info, /new, /quit");
        msg
    }

    /// Gera sugestões iniciais baseadas no perfil (para onboarding).
    pub fn initial_suggestions(&self) -> Vec<String> {
        match self.blackboard.get_tuple("config", "initial_suggestions") {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => vec![
                "Criar uma planilha".into(),
                "Organizar meus dados".into(),
                "Fazer um relatório".into(),
            ],
        }
    }

    /// Formata sugestões para exibição após resposta do assistente.
    pub fn format_suggestions(&self, suggestions: &[String]) -> String {
        if suggestions.is_empty() {
            return String::new();
        }
        format!("\n💡 Você também pode: {}", suggestions.join(" | "))
    }
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_help() -> HelpContextual {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        HelpContextual::new(bb)
    }

    #[test]
    fn sugere_para_task() {
        let h = temp_help();
        let suggs = h.suggest("Crie uma planilha de estoque");
        assert!(!suggs.is_empty());
        assert!(suggs.iter().any(|s| s.contains("mais alguma")));
    }

    #[test]
    fn sugere_para_conversacional() {
        let h = temp_help();
        let suggs = h.suggest("O que é Excel?");
        assert!(!suggs.is_empty());
        assert!(suggs
            .iter()
            .any(|s| s.contains("exemplo") || s.contains("tarefa")));
    }

    #[test]
    fn limita_a_3_sugestoes() {
        let h = temp_help();
        let suggs = h.suggest("Preciso de ajuda com tudo");
        assert!(
            suggs.len() <= 3,
            "deve limitar a 3 sugestões, tem {}",
            suggs.len()
        );
    }

    #[test]
    fn help_message_com_contexto() {
        let h = temp_help();
        let msg = h.help_message(Some("Crie uma planilha"));
        assert!(msg.contains("💡 Você pode:"));
        assert!(msg.contains("1."));
    }

    #[test]
    fn help_message_sem_contexto() {
        let h = temp_help();
        let msg = h.help_message(None);
        assert!(msg.contains("💡 Você pode:"));
        assert!(msg.contains("pergunta"));
    }

    #[test]
    fn format_suggestions_vazio() {
        let h = temp_help();
        let formatted = h.format_suggestions(&[]);
        assert!(formatted.is_empty());
    }

    #[test]
    fn format_suggestions_nao_vazio() {
        let h = temp_help();
        let formatted = h.format_suggestions(&["A".into(), "B".into()]);
        assert!(formatted.contains("💡"));
        assert!(formatted.contains("A"));
        assert!(formatted.contains("B"));
    }
}
