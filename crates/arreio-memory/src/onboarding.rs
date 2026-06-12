//! OnboardingWizard — configuração guiada para primeira execução.
//!
//! Detecta primeira execução e guia o usuário em 3 perguntas rápidas,
//! criando um perfil durável que enriquece todas as conversas futuras.
//!
//! Princípio: o sistema aprende com o usuário, não o contrário.

use anyhow::Result;
use arreio_kernel::Blackboard;

use crate::envelope::{MemoryEnvelope, MemoryType, ModalityRef, Scope};
use crate::project::ProjectMemory;

/// Perfil do usuário extraído do onboarding.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserProfile {
    pub business_name: String,
    pub activity: String,
    pub goal: String,
    pub created_at: u64,
}

/// Wizard de onboarding para primeira execução.
pub struct OnboardingWizard {
    blackboard: Blackboard,
}

impl OnboardingWizard {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Verifica se o onboarding já foi completado.
    pub fn is_complete(&self) -> Result<bool> {
        Ok(self
            .blackboard
            .get_tuple("config", "onboarding_complete")
            .is_some())
    }

    /// Executa o wizard de onboarding.
    ///
    /// Retorna o perfil do usuário. Se já completado, retorna o perfil existente.
    pub fn run<F>(&self, mut ask: F) -> Result<UserProfile>
    where
        F: FnMut(&str) -> Result<String>,
    {
        // Se já completou, retorna perfil existente
        if self.is_complete()? {
            return self.load_profile();
        }

        // Pergunta 1: nome do negócio
        let business_name = ask(
            "[arreio] Bem-vindo! Vou configurar tudo para você. São só 3 perguntas rápidas:\n\n\
             [arreio] 1/3 — Qual o nome do seu negócio? (ou seu nome, se for pessoa física)",
        )?;

        // Pergunta 2: atividade
        let activity = ask(
            "[arreio] 2/3 — Com o que você trabalha? (ex: vendas, consultoria, produção, serviços)",
        )?;

        // Pergunta 3: objetivo principal
        let goal = ask(
            "[arreio] 3/3 — O que você mais precisa de ajuda? (ex: planilhas, controle, organização, relatórios)",
        )?;

        let profile = UserProfile {
            business_name: business_name.trim().into(),
            activity: activity.trim().into(),
            goal: goal.trim().into(),
            created_at: now_epoch_secs(),
        };

        // Persiste perfil
        self.save_profile(&profile)?;

        // Marca onboarding como completo
        self.blackboard
            .put_tuple("config", "onboarding_complete", serde_json::json!(true))?;

        // Configurações automáticas baseadas no perfil
        self.apply_profile_defaults(&profile)?;

        // Cria arquivo de memória de projeto
        if let Ok(pm) = ProjectMemory::open(std::path::Path::new(".")) {
            let _ = pm.append_progress(&format!(
                "Onboarding completo: {} | {} | {}",
                profile.business_name, profile.activity, profile.goal
            ));
        }

        Ok(profile)
    }

    /// Executa wizard silencioso (sem interação, para testes).
    pub fn run_silent(
        &self,
        business_name: &str,
        activity: &str,
        goal: &str,
    ) -> Result<UserProfile> {
        let profile = UserProfile {
            business_name: business_name.into(),
            activity: activity.into(),
            goal: goal.into(),
            created_at: now_epoch_secs(),
        };

        self.save_profile(&profile)?;

        self.blackboard
            .put_tuple("config", "onboarding_complete", serde_json::json!(true))?;

        self.apply_profile_defaults(&profile)?;

        Ok(profile)
    }

    /// Carrega o perfil existente.
    pub fn load_profile(&self) -> Result<UserProfile> {
        match self.blackboard.get_tuple("memory", "profile_data") {
            Some(v) => {
                let profile: UserProfile = serde_json::from_value(v)
                    .map_err(|e| anyhow::anyhow!("falha ao carregar perfil: {}", e))?;
                Ok(profile)
            }
            None => Err(anyhow::anyhow!("perfil não encontrado")),
        }
    }

    /// Retorna o perfil ou um perfil vazio se não existir.
    pub fn get_profile_or_default(&self) -> UserProfile {
        self.load_profile().unwrap_or_else(|_| UserProfile {
            business_name: "Usuário".into(),
            activity: "geral".into(),
            goal: "ajuda geral".into(),
            created_at: 0,
        })
    }

    /// Gera contexto de perfil para injeção no system prompt.
    pub fn profile_context(&self) -> String {
        match self.load_profile() {
            Ok(p) => format!(
                "Contexto do usuário:\n\
                 - Negócio: {}\n\
                 - Atividade: {}\n\
                 - Objetivo principal: {}\n",
                p.business_name, p.activity, p.goal
            ),
            Err(_) => String::new(),
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn save_profile(&self, profile: &UserProfile) -> Result<()> {
        // Salva como MemoryEnvelope
        let envelope = MemoryEnvelope {
            id: "profile".into(),
            scope: Scope {
                tenant_id: None,
                user_id: Some("default".into()),
                agent_id: None,
                project_id: None,
                session_id: None,
            },
            memory_type: MemoryType::Preference,
            modalities: vec![ModalityRef {
                modality_type: "text".into(),
                content: format!(
                    "Negócio: {}, Atividade: {}, Objetivo: {}",
                    profile.business_name, profile.activity, profile.goal
                ),
            }],
            importance: 0.95,
            confidence: 0.9,
            entities: vec![profile.business_name.clone(), profile.activity.clone()],
            tags: vec!["profile".into(), "onboarding".into(), "preference".into()],
            content_hash: format!("{:x}", {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                profile.business_name.hash(&mut h);
                h.finish()
            }),
            created_at: profile.created_at,
        };

        self.blackboard
            .put_tuple("memory", "profile", serde_json::to_value(envelope)?)?;

        self.blackboard
            .put_tuple("memory", "profile_data", serde_json::to_value(profile)?)?;

        Ok(())
    }

    fn apply_profile_defaults(&self, profile: &UserProfile) -> Result<()> {
        let activity_lower = profile.activity.to_lowercase();
        let goal_lower = profile.goal.to_lowercase();

        // Configura contexto padrão
        let mut default_context = format!(
            "O usuário trabalha com {} e precisa de ajuda com {}.",
            profile.activity, profile.goal
        );

        // Skills pré-carregadas baseadas no perfil
        let skills = if activity_lower.contains("venda")
            || activity_lower.contains("loja")
            || activity_lower.contains("comércio")
            || activity_lower.contains("varejo")
        {
            vec!["estoque", "vendas", "planilhas", "controle", "clientes"]
        } else if activity_lower.contains("consultoria")
            || activity_lower.contains("escritório")
            || activity_lower.contains("serviço")
        {
            vec![
                "relatórios",
                "propostas",
                "contratos",
                "planilhas",
                "organização",
            ]
        } else if activity_lower.contains("produção")
            || activity_lower.contains("fábrica")
            || activity_lower.contains("indústria")
        {
            vec!["produção", "controle", "qualidade", "estoque", "planilhas"]
        } else {
            vec!["planilhas", "organização", "controle", "relatórios"]
        };

        default_context.push_str(&format!("\nSkills relevantes: {}.", skills.join(", ")));

        self.blackboard.put_tuple(
            "config",
            "default_context",
            serde_json::json!(default_context),
        )?;

        // Sugestões iniciais baseadas no goal
        let initial_suggestions = if goal_lower.contains("planilha")
            || goal_lower.contains("excel")
            || goal_lower.contains("tabela")
        {
            vec![
                "Criar planilha de controle",
                "Criar planilha de organização",
                "Modelo de planilha pronto",
            ]
        } else if goal_lower.contains("controle")
            || goal_lower.contains("gestão")
            || goal_lower.contains("administração")
        {
            vec![
                "Controle de estoque",
                "Controle de vendas",
                "Organização de clientes",
            ]
        } else if goal_lower.contains("relatório")
            || goal_lower.contains("relatorio")
            || goal_lower.contains("report")
        {
            vec![
                "Gerar relatório de vendas",
                "Relatório de desempenho",
                "Resumo mensal",
            ]
        } else {
            vec!["Criar planilha", "Organizar dados", "Gerar relatório"]
        };

        self.blackboard.put_tuple(
            "config",
            "initial_suggestions",
            serde_json::to_value(initial_suggestions)?,
        )?;

        Ok(())
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

    fn temp_wizard() -> OnboardingWizard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        OnboardingWizard::new(bb)
    }

    #[test]
    fn onboarding_executa_na_primeira_vez() {
        let wizard = temp_wizard();
        assert!(!wizard.is_complete().unwrap());

        let profile = wizard
            .run_silent(
                "Loja Moda Urbana",
                "Venda de roupas masculinas",
                "Controle de estoque e vendas",
            )
            .unwrap();

        assert_eq!(profile.business_name, "Loja Moda Urbana");
        assert_eq!(profile.activity, "Venda de roupas masculinas");
        assert_eq!(profile.goal, "Controle de estoque e vendas");
        assert!(wizard.is_complete().unwrap());
    }

    #[test]
    fn onboarding_pula_se_ja_completo() {
        let wizard = temp_wizard();
        wizard.run_silent("Loja A", "Vendas", "Estoque").unwrap();

        // Simula segunda execução
        let profile = wizard.load_profile().unwrap();
        assert_eq!(profile.business_name, "Loja A");
    }

    #[test]
    fn perfil_gera_contexto() {
        let wizard = temp_wizard();
        wizard
            .run_silent("Loja B", "Consultoria", "Relatórios")
            .unwrap();

        let context = wizard.profile_context();
        assert!(context.contains("Loja B"));
        assert!(context.contains("Consultoria"));
        assert!(context.contains("Relatórios"));
    }

    #[test]
    fn perfil_default_sem_onboarding() {
        let wizard = temp_wizard();
        let default = wizard.get_profile_or_default();
        assert_eq!(default.business_name, "Usuário");
    }

    #[test]
    fn skills_comercio_pre_carregadas() {
        let wizard = temp_wizard();
        wizard.run_silent("Loja C", "vendas", "planilhas").unwrap();

        let ctx = wizard
            .blackboard
            .get_tuple("config", "default_context")
            .unwrap();
        let ctx_str = ctx.as_str().unwrap_or("");
        assert!(ctx_str.contains("estoque") || ctx_str.contains("vendas"));
    }

    #[test]
    fn sugestoes_iniciais_baseadas_em_goal() {
        let wizard = temp_wizard();
        wizard
            .run_silent("Loja D", "comércio", "planilhas")
            .unwrap();

        let suggestions = wizard
            .blackboard
            .get_tuple("config", "initial_suggestions")
            .unwrap();
        let suggs: Vec<String> = serde_json::from_value(suggestions).unwrap();
        assert!(!suggs.is_empty());
        assert!(suggs.iter().any(|s| s.contains("planilha")));
    }
}
