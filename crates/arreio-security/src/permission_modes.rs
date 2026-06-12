//! 7 Modos de Permissão — Graduated Trust Spectrum (GAP-010).
//!
//! Define 7 modos de permissão que controlam o nível de autonomia
//! do sistema ao executar ferramentas. Cada modo mapeia para regras
//! explícitas sobre quais categorias de ferramentas são permitidas,
//! escalonadas ou negadas.

use serde::{Deserialize, Serialize};

/// Identificador dos 7 modos de permissão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionModeId {
    /// Modo padrão: reads aprovados, writes escalonados, exec escalonado.
    Default,
    /// Modo planejamento: reads e writes no workspace aprovados, exec bloqueado.
    Plan,
    /// Aceita edições: reads e writes aprovados, exec escalonado.
    AcceptEdits,
    /// Não perguntar: tudo aprovado exceto comandos destrutivos.
    DontAsk,
    /// Automático: tudo aprovado, sem intervenção humana.
    Auto,
    /// Automático com classificador: usa YoloClassifier para decidir.
    AutoWithClassifier,
    /// Bypass: sem restrições (apenas para desenvolvimento/debug).
    Bypass,
}

impl PermissionModeId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AcceptEdits => "accept-edits",
            Self::DontAsk => "dont-ask",
            Self::Auto => "auto",
            Self::AutoWithClassifier => "auto-classifier",
            Self::Bypass => "bypass",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "default" => Some(Self::Default),
            "plan" => Some(Self::Plan),
            "accept-edits" | "acceptedits" => Some(Self::AcceptEdits),
            "dont-ask" | "dontask" => Some(Self::DontAsk),
            "auto" => Some(Self::Auto),
            "auto-classifier" | "autoclassifier" | "auto-with-classifier" => {
                Some(Self::AutoWithClassifier)
            }
            "bypass" => Some(Self::Bypass),
            _ => None,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Default => "Reads aprovados, writes e exec escalonados para aprovação",
            Self::Plan => "Reads e writes no workspace aprovados, exec bloqueado",
            Self::AcceptEdits => "Reads e writes aprovados, exec escalonado",
            Self::DontAsk => "Tudo aprovado exceto comandos destrutivos conhecidos",
            Self::Auto => "Execução autônoma completa sem intervenção",
            Self::AutoWithClassifier => "Execução autônoma com classificador heurístico",
            Self::Bypass => "Sem restrições (uso em desenvolvimento apenas)",
        }
    }

    pub fn all() -> &'static [PermissionModeId] {
        &[
            Self::Default,
            Self::Plan,
            Self::AcceptEdits,
            Self::DontAsk,
            Self::Auto,
            Self::AutoWithClassifier,
            Self::Bypass,
        ]
    }
}

/// Categorias de ferramentas para autorização.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    Read,
    Write,
    Exec,
    Destructive,
    Network,
    Rollback,
    Unknown,
}

impl ToolCategory {
    /// Classifica uma ferramenta pelo nome.
    pub fn from_tool_name(name: &str) -> Self {
        match name {
            "read_file" | "grep_search" | "glob_search" | "list_dir" | "memory_search"
            | "describe_image" | "transcribe_audio" => Self::Read,

            "write_file" | "edit_file" | "apply_patch" => Self::Write,

            "exec" | "bash" | "shell" => Self::Exec,

            "checkpoint_rollback" => Self::Rollback,

            "web_search" | "web_fetch" => Self::Network,

            _ => Self::Unknown,
        }
    }
}

/// Resultado da autorização por modo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeAuthorization {
    Allow,
    Escalate,
    Deny,
}

/// Especificação completa de um modo de permissão.
pub struct PermissionModeSpec {
    pub id: PermissionModeId,
}

impl PermissionModeSpec {
    pub fn new(id: PermissionModeId) -> Self {
        Self { id }
    }

    /// Autoriza uma ferramenta baseado no modo ativo.
    pub fn authorize(&self, tool_name: &str) -> ModeAuthorization {
        let category = ToolCategory::from_tool_name(tool_name);
        self.authorize_category(category)
    }

    /// Autoriza uma categoria de ferramenta.
    pub fn authorize_category(&self, category: ToolCategory) -> ModeAuthorization {
        match self.id {
            PermissionModeId::Default => match category {
                ToolCategory::Read => ModeAuthorization::Allow,
                ToolCategory::Write => ModeAuthorization::Escalate,
                ToolCategory::Exec => ModeAuthorization::Escalate,
                ToolCategory::Destructive => ModeAuthorization::Deny,
                ToolCategory::Network => ModeAuthorization::Allow,
                ToolCategory::Rollback => ModeAuthorization::Escalate,
                ToolCategory::Unknown => ModeAuthorization::Escalate,
            },

            PermissionModeId::Plan => match category {
                ToolCategory::Read => ModeAuthorization::Allow,
                ToolCategory::Write => ModeAuthorization::Allow,
                ToolCategory::Exec => ModeAuthorization::Deny,
                ToolCategory::Destructive => ModeAuthorization::Deny,
                ToolCategory::Network => ModeAuthorization::Escalate,
                ToolCategory::Rollback => ModeAuthorization::Deny,
                ToolCategory::Unknown => ModeAuthorization::Deny,
            },

            PermissionModeId::AcceptEdits => match category {
                ToolCategory::Read => ModeAuthorization::Allow,
                ToolCategory::Write => ModeAuthorization::Allow,
                ToolCategory::Exec => ModeAuthorization::Escalate,
                ToolCategory::Destructive => ModeAuthorization::Deny,
                ToolCategory::Network => ModeAuthorization::Allow,
                ToolCategory::Rollback => ModeAuthorization::Escalate,
                ToolCategory::Unknown => ModeAuthorization::Escalate,
            },

            PermissionModeId::DontAsk => match category {
                ToolCategory::Read => ModeAuthorization::Allow,
                ToolCategory::Write => ModeAuthorization::Allow,
                ToolCategory::Exec => ModeAuthorization::Allow,
                ToolCategory::Destructive => ModeAuthorization::Escalate,
                ToolCategory::Network => ModeAuthorization::Allow,
                ToolCategory::Rollback => ModeAuthorization::Allow,
                ToolCategory::Unknown => ModeAuthorization::Allow,
            },

            PermissionModeId::Auto => match category {
                ToolCategory::Read => ModeAuthorization::Allow,
                ToolCategory::Write => ModeAuthorization::Allow,
                ToolCategory::Exec => ModeAuthorization::Allow,
                ToolCategory::Destructive => ModeAuthorization::Allow,
                ToolCategory::Network => ModeAuthorization::Allow,
                ToolCategory::Rollback => ModeAuthorization::Allow,
                ToolCategory::Unknown => ModeAuthorization::Allow,
            },

            PermissionModeId::AutoWithClassifier => {
                // Neste modo, a decisão é delegada ao YoloClassifier.
                // Aqui retornamos Escalate para que o caller use o classificador.
                match category {
                    ToolCategory::Read => ModeAuthorization::Allow,
                    ToolCategory::Destructive => ModeAuthorization::Deny,
                    _ => ModeAuthorization::Escalate,
                }
            }

            PermissionModeId::Bypass => ModeAuthorization::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Default mode ────────────────────────────────────────────────────────

    #[test]
    fn default_allows_read() {
        let spec = PermissionModeSpec::new(PermissionModeId::Default);
        assert_eq!(spec.authorize("read_file"), ModeAuthorization::Allow);
        assert_eq!(spec.authorize("grep_search"), ModeAuthorization::Allow);
    }

    #[test]
    fn default_escalates_write() {
        let spec = PermissionModeSpec::new(PermissionModeId::Default);
        assert_eq!(spec.authorize("write_file"), ModeAuthorization::Escalate);
        assert_eq!(spec.authorize("edit_file"), ModeAuthorization::Escalate);
    }

    #[test]
    fn default_escalates_exec() {
        let spec = PermissionModeSpec::new(PermissionModeId::Default);
        assert_eq!(spec.authorize("exec"), ModeAuthorization::Escalate);
    }

    // ── Plan mode ───────────────────────────────────────────────────────────

    #[test]
    fn plan_allows_write() {
        let spec = PermissionModeSpec::new(PermissionModeId::Plan);
        assert_eq!(spec.authorize("write_file"), ModeAuthorization::Allow);
    }

    #[test]
    fn plan_denies_exec() {
        let spec = PermissionModeSpec::new(PermissionModeId::Plan);
        assert_eq!(spec.authorize("exec"), ModeAuthorization::Deny);
    }

    // ── AcceptEdits mode ────────────────────────────────────────────────────

    #[test]
    fn accept_edits_allows_write() {
        let spec = PermissionModeSpec::new(PermissionModeId::AcceptEdits);
        assert_eq!(spec.authorize("write_file"), ModeAuthorization::Allow);
        assert_eq!(spec.authorize("edit_file"), ModeAuthorization::Allow);
    }

    #[test]
    fn accept_edits_escalates_exec() {
        let spec = PermissionModeSpec::new(PermissionModeId::AcceptEdits);
        assert_eq!(spec.authorize("exec"), ModeAuthorization::Escalate);
    }

    // ── DontAsk mode ────────────────────────────────────────────────────────

    #[test]
    fn dontask_allows_exec() {
        let spec = PermissionModeSpec::new(PermissionModeId::DontAsk);
        assert_eq!(spec.authorize("exec"), ModeAuthorization::Allow);
        assert_eq!(spec.authorize("write_file"), ModeAuthorization::Allow);
    }

    // ── Auto mode ───────────────────────────────────────────────────────────

    #[test]
    fn auto_allows_everything() {
        let spec = PermissionModeSpec::new(PermissionModeId::Auto);
        assert_eq!(spec.authorize("exec"), ModeAuthorization::Allow);
        assert_eq!(spec.authorize("write_file"), ModeAuthorization::Allow);
        assert_eq!(
            spec.authorize("checkpoint_rollback"),
            ModeAuthorization::Allow
        );
    }

    // ── AutoWithClassifier ──────────────────────────────────────────────────

    #[test]
    fn auto_classifier_denies_destructive() {
        let spec = PermissionModeSpec::new(PermissionModeId::AutoWithClassifier);
        assert_eq!(
            spec.authorize_category(ToolCategory::Destructive),
            ModeAuthorization::Deny
        );
    }

    #[test]
    fn auto_classifier_allows_read() {
        let spec = PermissionModeSpec::new(PermissionModeId::AutoWithClassifier);
        assert_eq!(spec.authorize("read_file"), ModeAuthorization::Allow);
    }

    // ── Bypass mode ─────────────────────────────────────────────────────────

    #[test]
    fn bypass_allows_everything() {
        let spec = PermissionModeSpec::new(PermissionModeId::Bypass);
        assert_eq!(spec.authorize("exec"), ModeAuthorization::Allow);
        assert_eq!(spec.authorize("write_file"), ModeAuthorization::Allow);
        assert_eq!(
            spec.authorize("checkpoint_rollback"),
            ModeAuthorization::Allow
        );
    }

    // ── Cada modo × cada categoria ──────────────────────────────────────────

    #[test]
    fn all_modes_x_read_category() {
        for mode_id in PermissionModeId::all() {
            let spec = PermissionModeSpec::new(*mode_id);
            assert_eq!(
                spec.authorize_category(ToolCategory::Read),
                ModeAuthorization::Allow,
                "modo {:?} deveria permitir reads",
                mode_id
            );
        }
    }

    // ── PermissionModeId parsing ────────────────────────────────────────────

    #[test]
    fn mode_id_from_str() {
        assert_eq!(
            PermissionModeId::from_str("default"),
            Some(PermissionModeId::Default)
        );
        assert_eq!(
            PermissionModeId::from_str("plan"),
            Some(PermissionModeId::Plan)
        );
        assert_eq!(
            PermissionModeId::from_str("accept-edits"),
            Some(PermissionModeId::AcceptEdits)
        );
        assert_eq!(
            PermissionModeId::from_str("dont-ask"),
            Some(PermissionModeId::DontAsk)
        );
        assert_eq!(
            PermissionModeId::from_str("auto"),
            Some(PermissionModeId::Auto)
        );
        assert_eq!(
            PermissionModeId::from_str("auto-classifier"),
            Some(PermissionModeId::AutoWithClassifier)
        );
        assert_eq!(
            PermissionModeId::from_str("bypass"),
            Some(PermissionModeId::Bypass)
        );
        assert_eq!(PermissionModeId::from_str("invalid"), None);
    }

    #[test]
    fn mode_id_roundtrip() {
        for mode in PermissionModeId::all() {
            assert_eq!(PermissionModeId::from_str(mode.as_str()), Some(*mode));
        }
    }

    // ── ToolCategory classification ─────────────────────────────────────────

    #[test]
    fn tool_category_classification() {
        assert_eq!(
            ToolCategory::from_tool_name("read_file"),
            ToolCategory::Read
        );
        assert_eq!(
            ToolCategory::from_tool_name("write_file"),
            ToolCategory::Write
        );
        assert_eq!(ToolCategory::from_tool_name("exec"), ToolCategory::Exec);
        assert_eq!(
            ToolCategory::from_tool_name("checkpoint_rollback"),
            ToolCategory::Rollback
        );
        assert_eq!(
            ToolCategory::from_tool_name("web_search"),
            ToolCategory::Network
        );
        assert_eq!(
            ToolCategory::from_tool_name("mcp_custom"),
            ToolCategory::Unknown
        );
    }
}
