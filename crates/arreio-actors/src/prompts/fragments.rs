//! Fragmentos condicionais do system prompt (GAP-026).
//!
//! Cada função retorna `Option<String>`: `Some` se o fragmento é relevante
//! para o SessionState atual, `None` caso contrário.

use super::{ActorRole, SessionState};

pub fn role_identity(state: &SessionState) -> Option<String> {
    Some(match state.actor_role {
        ActorRole::Architect => "\
Você é um Arquiteto de Sistemas especializado em decompor especificações técnicas em \
grafos de tarefas executáveis."
            .to_string(),
        ActorRole::Developer => "\
Você é um Programador especializado. Receba UMA tarefa e o contexto necessário para \
implementá-la."
            .to_string(),
        ActorRole::Inspector => "\
Você é um Inspetor de Segurança e Qualidade. Analise diffs de código com rigor."
            .to_string(),
    })
}

pub fn output_format(state: &SessionState) -> Option<String> {
    Some(match state.actor_role {
        ActorRole::Architect => "\
Retorne SOMENTE um JSON array. Cada elemento deve ter: {\"id\": string, \"title\": string, \
\"depends_on\": [string], \"actor_type\": \"developer\"|\"inspector\", \
\"file_target\": string|null, \"instruction\": string}. Zero explicações fora do JSON."
            .to_string(),
        ActorRole::Developer => "\
Retorne SOMENTE o código modificado completo, sem explicações, sem markdown, sem \
blocos de código delimitados por ```. Apenas o código-fonte puro."
            .to_string(),
        ActorRole::Inspector => "\
Retorne SOMENTE JSON com formato: {\"approved\": bool, \"issues\": [string]}. \
Zero texto fora do JSON."
            .to_string(),
    })
}

pub fn security_constraints(state: &SessionState) -> Option<String> {
    if state.actor_role != ActorRole::Inspector {
        return None;
    }
    Some(
        "\
Bloqueie: injeção de comandos, credenciais hardcoded, remoção de autenticação, \
SQL injection, loops infinitos sem escape."
            .to_string(),
    )
}

pub fn code_conventions(state: &SessionState) -> Option<String> {
    if state.actor_role != ActorRole::Developer {
        return None;
    }
    Some(
        "\
Preserve o estilo do código existente. Não adicione dependências sem necessidade. \
Não refatore fora do escopo da tarefa."
            .to_string(),
    )
}

pub fn token_economy(_state: &SessionState) -> Option<String> {
    Some("Minimize tokens de saída. Sem explicações ou texto não solicitado.".to_string())
}

pub fn error_handling(state: &SessionState) -> Option<String> {
    if state.actor_role == ActorRole::Architect {
        return None;
    }
    Some("Se encontrar erro irrecuperável, retorne a resposta no formato esperado com o erro descrito.".to_string())
}

pub fn checkpoint_awareness(state: &SessionState) -> Option<String> {
    if state.actor_role != ActorRole::Developer {
        return None;
    }
    Some(
        "O sistema mantém checkpoints git automáticos. Não se preocupe com rollback manual."
            .to_string(),
    )
}

pub fn permission_mode_hint(state: &SessionState) -> Option<String> {
    if state.permission_mode.is_empty() || state.actor_role == ActorRole::Architect {
        return None;
    }
    Some(format!(
        "Modo de permissão ativo: {}. Ferramentas destrutivas podem ser bloqueadas.",
        state.permission_mode
    ))
}

pub fn tool_use_hint(state: &SessionState) -> Option<String> {
    if !state.has_tools || state.actor_role != ActorRole::Developer {
        return None;
    }
    Some(
        "\
Ferramentas disponíveis. Use read_file antes de modificar. Use grep_search ou glob_search \
para localizar código. Quando a tarefa estiver concluída, retorne APENAS o código final."
            .to_string(),
    )
}

pub fn ast_context_hint(state: &SessionState) -> Option<String> {
    if !state.has_ast_map {
        return None;
    }
    Some(
        "Mapa AST do arquivo alvo disponível no contexto. Use as assinaturas para navegar."
            .to_string(),
    )
}

pub fn memory_context_hint(state: &SessionState) -> Option<String> {
    if !state.has_memory {
        return None;
    }
    Some(
        "Memória de projeto recuperada disponível. Considere resoluções anteriores semelhantes."
            .to_string(),
    )
}

pub fn skills_hint(state: &SessionState) -> Option<String> {
    if !state.has_skills {
        return None;
    }
    Some("Skills aprendidas disponíveis no contexto. Reutilize padrões já validados.".to_string())
}

pub fn agents_md_hint(state: &SessionState) -> Option<String> {
    if !state.has_agents_md {
        return None;
    }
    Some(
        "Instruções do projeto (AGENTS.md) disponíveis. Siga as convenções especificadas."
            .to_string(),
    )
}

pub fn workspace_boundary(state: &SessionState) -> Option<String> {
    state.workspace_path.as_ref().map(|p| {
        format!(
            "Workspace root: {}. Não modifique arquivos fora deste diretório.",
            p
        )
    })
}

pub fn environment_hints(state: &SessionState) -> Option<String> {
    let mut hints = Vec::new();
    if state.is_docker {
        hints.push("Executando dentro de container Docker.");
    }
    if state.is_wsl {
        hints.push("Executando em WSL (Windows Subsystem for Linux).");
    }
    if hints.is_empty() {
        None
    } else {
        Some(hints.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev_state() -> SessionState {
        SessionState {
            actor_role: ActorRole::Developer,
            ..Default::default()
        }
    }

    fn inspector_state() -> SessionState {
        SessionState {
            actor_role: ActorRole::Inspector,
            ..Default::default()
        }
    }

    #[test]
    fn role_identity_varia_por_ator() {
        let dev = role_identity(&dev_state()).unwrap();
        let insp = role_identity(&inspector_state()).unwrap();
        assert!(dev.contains("Programador"));
        assert!(insp.contains("Inspetor"));
    }

    #[test]
    fn security_constraints_so_para_inspector() {
        assert!(security_constraints(&inspector_state()).is_some());
        assert!(security_constraints(&dev_state()).is_none());
    }

    #[test]
    fn tool_use_hint_condicional() {
        let mut state = dev_state();
        assert!(tool_use_hint(&state).is_none());
        state.has_tools = true;
        assert!(tool_use_hint(&state).is_some());
    }

    #[test]
    fn environment_hints_vazio_quando_local() {
        assert!(environment_hints(&dev_state()).is_none());
    }

    #[test]
    fn environment_hints_presente_em_docker() {
        let mut state = dev_state();
        state.is_docker = true;
        let hint = environment_hints(&state).unwrap();
        assert!(hint.contains("Docker"));
    }

    #[test]
    fn workspace_boundary_none_sem_path() {
        assert!(workspace_boundary(&dev_state()).is_none());
    }

    #[test]
    fn workspace_boundary_some_com_path() {
        let mut state = dev_state();
        state.workspace_path = Some("/home/user/project".into());
        let hint = workspace_boundary(&state).unwrap();
        assert!(hint.contains("/home/user/project"));
    }
}
