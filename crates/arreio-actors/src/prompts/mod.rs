//! System Prompt Assembly dinâmico (GAP-026).
//!
//! Substitui constantes estáticas ARCHITECT_SYSTEM, DEVELOPER_SYSTEM e INSPECTOR_SYSTEM
//! por montagem condicional baseada em SessionState.
//!
//! O marcador `DYNAMIC_BOUNDARY` separa a seção cacheável (frozen + semi-frozen)
//! da seção volátil (muda a cada chamada), permitindo prefix caching eficiente.

pub mod fragments;

use crate::ActorContext;

/// Marcador que separa seção cacheável da volátil no system prompt montado.
pub const DYNAMIC_BOUNDARY: &str = "\n__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__\n";

/// Estado da sessão que condiciona quais fragmentos são incluídos.
#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub actor_role: ActorRole,
    pub has_tools: bool,
    pub has_ast_map: bool,
    pub has_memory: bool,
    pub has_skills: bool,
    pub has_agents_md: bool,
    pub permission_mode: String,
    pub model_name: String,
    pub workspace_path: Option<String>,
    pub is_docker: bool,
    pub is_wsl: bool,
    pub fsm_state: String,
}

impl SessionState {
    /// Deriva SessionState a partir de um ActorContext + role.
    pub fn from_context(ctx: &ActorContext, role: ActorRole) -> Self {
        Self {
            actor_role: role,
            has_tools: false,
            has_ast_map: ctx.ast_map.is_some(),
            has_memory: ctx.memory_frame.is_some(),
            has_skills: !ctx.skills_context.is_empty(),
            has_agents_md: ctx.agents_md.is_some(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActorRole {
    Architect,
    #[default]
    Developer,
    Inspector,
}

/// Monta o system prompt base para um ator.
///
/// O prompt é composto por fragmentos condicionais baseados no SessionState.
/// O marcador `DYNAMIC_BOUNDARY` separa a seção estável (acima, cacheável)
/// da seção volátil (abaixo, muda por chamada).
pub fn assemble_system_prompt(state: &SessionState) -> String {
    let mut cacheable = Vec::new();
    let mut volatile = Vec::new();

    // Fragmentos estáveis (cacheable — dependem apenas do role)
    if let Some(f) = fragments::role_identity(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::output_format(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::security_constraints(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::code_conventions(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::token_economy(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::error_handling(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::checkpoint_awareness(state) {
        cacheable.push(f);
    }
    if let Some(f) = fragments::permission_mode_hint(state) {
        cacheable.push(f);
    }

    // Fragmentos voláteis (mudam entre chamadas)
    if let Some(f) = fragments::tool_use_hint(state) {
        volatile.push(f);
    }
    if let Some(f) = fragments::ast_context_hint(state) {
        volatile.push(f);
    }
    if let Some(f) = fragments::memory_context_hint(state) {
        volatile.push(f);
    }
    if let Some(f) = fragments::skills_hint(state) {
        volatile.push(f);
    }
    if let Some(f) = fragments::agents_md_hint(state) {
        volatile.push(f);
    }
    if let Some(f) = fragments::workspace_boundary(state) {
        volatile.push(f);
    }
    if let Some(f) = fragments::environment_hints(state) {
        volatile.push(f);
    }

    let mut result = cacheable.join("\n");

    if !volatile.is_empty() {
        result.push_str(DYNAMIC_BOUNDARY);
        result.push_str(&volatile.join("\n"));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_developer_contem_fragmentos_basicos() {
        let state = SessionState {
            actor_role: ActorRole::Developer,
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(prompt.contains("Programador"));
        assert!(prompt.contains("código modificado completo"));
        assert!(prompt.contains("checkpoint"));
        assert!(prompt.contains("Minimize tokens"));
    }

    #[test]
    fn assemble_architect_sem_security_constraints() {
        let state = SessionState {
            actor_role: ActorRole::Architect,
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(prompt.contains("Arquiteto"));
        assert!(prompt.contains("JSON array"));
        assert!(!prompt.contains("Bloqueie"));
    }

    #[test]
    fn assemble_inspector_com_security() {
        let state = SessionState {
            actor_role: ActorRole::Inspector,
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(prompt.contains("Inspetor"));
        assert!(prompt.contains("Bloqueie"));
        assert!(prompt.contains("injeção de comandos"));
    }

    #[test]
    fn boundary_presente_quando_ha_volatile() {
        let state = SessionState {
            actor_role: ActorRole::Developer,
            has_tools: true,
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(prompt.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));
        assert!(prompt.contains("Ferramentas disponíveis"));
    }

    #[test]
    fn boundary_ausente_quando_sem_volatile() {
        let state = SessionState {
            actor_role: ActorRole::Architect,
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(!prompt.contains("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__"));
    }

    #[test]
    fn assemble_com_todos_volatiles() {
        let state = SessionState {
            actor_role: ActorRole::Developer,
            has_tools: true,
            has_ast_map: true,
            has_memory: true,
            has_skills: true,
            has_agents_md: true,
            workspace_path: Some("/project".into()),
            is_docker: true,
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(prompt.contains("Ferramentas disponíveis"));
        assert!(prompt.contains("Mapa AST"));
        assert!(prompt.contains("Memória de projeto"));
        assert!(prompt.contains("Skills aprendidas"));
        assert!(prompt.contains("AGENTS.md"));
        assert!(prompt.contains("/project"));
        assert!(prompt.contains("Docker"));
    }

    #[test]
    fn from_context_deriva_corretamente() {
        let ctx = ActorContext {
            task_payload: serde_json::json!({}),
            ast_map: Some("symbols".into()),
            memory_frame: None,
            skills_context: "skill: fix_auth".into(),
            agents_md: Some("## Rules".into()),
            architect_rationale: None,
            dependencies_summary: None,
            parent_spec: None,
            retry_context: None,
            trajectory_window: None,
        };
        let state = SessionState::from_context(&ctx, ActorRole::Developer);
        assert!(state.has_ast_map);
        assert!(!state.has_memory);
        assert!(state.has_skills);
        assert!(state.has_agents_md);
        assert_eq!(state.actor_role, ActorRole::Developer);
    }

    #[test]
    fn permission_mode_incluido_quando_definido() {
        let state = SessionState {
            actor_role: ActorRole::Developer,
            permission_mode: "WorkspaceWrite".into(),
            ..Default::default()
        };
        let prompt = assemble_system_prompt(&state);
        assert!(prompt.contains("WorkspaceWrite"));
    }
}
