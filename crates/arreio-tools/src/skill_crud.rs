//! Skill CRUD Tools — Ferramentas para o Developer criar/atualizar/remover skills
//! durante o tool-use loop. Skills criadas pelo agente nascem Untrusted e passam
//! pelo SkillValidator antes de serem persistidas.
//!
//! Inspirado no padrão de "store CRUD como ferramentas" do Continual Harness.

use arreio_kernel::Blackboard;
use arreio_skills::{Skill, SkillStore, SkillTrust};
use serde::Deserialize;
use serde_json::Value;

// ── Params structs ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SkillCreateParams {
    pub name: String,
    pub description: String,
    pub trigger_patterns: Vec<String>,
    pub instruction_template: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub validation_cmds: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SkillUpdateParams {
    pub name: String,
    pub description: Option<String>,
    pub steps: Option<Vec<String>>,
    pub instruction_template: Option<String>,
    pub validation_cmds: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SkillDeleteParams {
    pub name: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// Handler para skill_create.
/// Valida a skill via SkillValidator antes de persistir.
pub fn handle_skill_create(bb: &Blackboard, params: Value) -> Result<Value, String> {
    let params: SkillCreateParams =
        serde_json::from_value(params).map_err(|e| format!("Parâmetros inválidos: {}", e))?;

    let store = SkillStore::new(bb.clone());
    let validator = arreio_skills::SkillValidator::new();

    // Verifica se já existe skill com este nome
    if store.get(&params.name).is_some() {
        return Err(format!(
            "Skill '{}' já existe. Use skill_update para modificá-la.",
            params.name
        ));
    }

    let skill = Skill {
        name: params.name.clone(),
        description: params.description.clone(),
        trigger_patterns: params.trigger_patterns.clone(),
        instruction_template: params.instruction_template.clone(),
        steps: params.steps.clone(),
        templates: Default::default(),
        validation_cmds: params.validation_cmds.clone(),
        ast_signature: None,
        file_target_pattern: None,
        last_used: 0,
        usage_count: 1,
        success_rate: 1.0,
        created_from_dag_task_id: None,
        anti_conversation: true,
        idempotent: false,
        error_budget: 3,
        output_schema: None,
        allowed_tools: vec![],
        trust_level: SkillTrust::Untrusted,
        module_count: params.steps.len().max(1) as u32,
        mutation_history: vec![],
    };

    // Valida ANTES de salvar
    let (passed, results) = validator.validate(&skill);
    if !passed {
        let errors: Vec<String> = results
            .iter()
            .filter(|r| {
                !r.passed
                    && (r.severity == arreio_skills::ValidationSeverity::Critical
                        || r.severity == arreio_skills::ValidationSeverity::Warning)
            })
            .map(|r| r.message.clone())
            .collect();
        if !errors.is_empty() {
            return Err(format!("Validação falhou: {}", errors.join("; ")));
        }
    }

    store
        .save(&skill)
        .map_err(|e| format!("Erro ao salvar skill: {}", e))?;

    Ok(serde_json::json!({
        "status": "ok",
        "message": format!("Skill '{}' criada com sucesso (trust_level: Untrusted)", params.name),
        "skill_name": params.name,
    }))
}

/// Handler para skill_update.
/// Usa update_skill para registrar mutation history automaticamente.
pub fn handle_skill_update(bb: &Blackboard, params: Value) -> Result<Value, String> {
    let params: SkillUpdateParams =
        serde_json::from_value(params).map_err(|e| format!("Parâmetros inválidos: {}", e))?;

    let store = SkillStore::new(bb.clone());

    store
        .update_skill(
            &params.name,
            "developer-agent",
            Some("tool-use update"),
            |skill| {
                if let Some(ref desc) = params.description {
                    skill.description = desc.clone();
                }
                if let Some(ref steps) = params.steps {
                    skill.steps = steps.clone();
                    skill.module_count = steps.len().max(1) as u32;
                }
                if let Some(ref tmpl) = params.instruction_template {
                    skill.instruction_template = tmpl.clone();
                }
                if let Some(ref cmds) = params.validation_cmds {
                    skill.validation_cmds = cmds.clone();
                }
                skill.last_used = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            },
        )
        .map_err(|e| format!("Erro ao atualizar skill: {}", e))?;

    Ok(serde_json::json!({
        "status": "ok",
        "message": format!("Skill '{}' atualizada com sucesso", params.name),
    }))
}

/// Handler para skill_delete (soft-delete).
pub fn handle_skill_delete(bb: &Blackboard, params: Value) -> Result<Value, String> {
    let params: SkillDeleteParams =
        serde_json::from_value(params).map_err(|e| format!("Parâmetros inválidos: {}", e))?;

    let store = SkillStore::new(bb.clone());
    store
        .remove(&params.name)
        .map_err(|e| format!("Erro ao remover skill: {}", e))?;

    Ok(serde_json::json!({
        "status": "ok",
        "message": format!("Skill '{}' removida (soft-delete)", params.name),
    }))
}

// ── Tool Schemas (para registro no ToolRegistry) ─────────────────────────

pub fn skill_create_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "description", "trigger_patterns", "instruction_template"],
        "properties": {
            "name": {"type": "string", "description": "Nome único da skill"},
            "description": {"type": "string", "description": "Descrição do que a skill faz"},
            "trigger_patterns": {"type": "array", "items": {"type": "string"}, "description": "Palavras-chave para ativação"},
            "instruction_template": {"type": "string", "description": "Template de instrução"},
            "steps": {"type": "array", "items": {"type": "string"}, "description": "Passos do workflow"},
            "validation_cmds": {"type": "array", "items": {"type": "string"}, "description": "Comandos de validação"}
        }
    })
}

pub fn skill_update_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": {"type": "string", "description": "Nome da skill a atualizar"},
            "description": {"type": "string", "description": "Nova descrição"},
            "steps": {"type": "array", "items": {"type": "string"}, "description": "Novos passos"},
            "instruction_template": {"type": "string", "description": "Novo template"},
            "validation_cmds": {"type": "array", "items": {"type": "string"}, "description": "Novos comandos de validação"}
        }
    })
}

pub fn skill_delete_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": {"type": "string", "description": "Nome da skill a remover"}
        }
    })
}

// ── Testes ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_skills::SkillTrust;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn skill_create_valida_antes_de_salvar() {
        let bb = temp_bb();
        let params = serde_json::json!({
            "name": "",
            "description": "test",
            "trigger_patterns": [],
            "instruction_template": "do it"
        });
        let result = handle_skill_create(&bb, params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Validação falhou"));
    }

    #[test]
    fn skill_create_sucesso_nasce_untrusted() {
        let bb = temp_bb();
        let params = serde_json::json!({
            "name": "my-skill",
            "description": "Uma skill criada pelo agente para teste",
            "trigger_patterns": ["test"],
            "instruction_template": "Execute o procedimento",
            "steps": ["Passo 1", "Passo 2"],
            "validation_cmds": ["cargo test"]
        });
        let result = handle_skill_create(&bb, params);
        assert!(result.is_ok());

        let store = SkillStore::new(bb);
        let skill = store.get("my-skill").unwrap();
        assert_eq!(skill.trust_level, SkillTrust::Untrusted);
        assert_eq!(skill.module_count, 2);
    }

    #[test]
    fn skill_create_rejeita_duplicata() {
        let bb = temp_bb();
        let params = serde_json::json!({
            "name": "dup-skill",
            "description": "Skill duplicada de teste",
            "trigger_patterns": ["test"],
            "instruction_template": "Execute"
        });
        let _ = handle_skill_create(&bb, params.clone());
        let result = handle_skill_create(&bb, params);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("já existe"));
    }

    #[test]
    fn skill_delete_soft_delete() {
        let bb = temp_bb();
        let store = SkillStore::new(bb.clone());
        let skill = Skill {
            name: "to-delete".into(),
            description: "Para deletar".into(),
            trigger_patterns: vec!["test".into()],
            instruction_template: "do".into(),
            steps: vec![],
            templates: Default::default(),
            validation_cmds: vec![],
            ast_signature: None,
            file_target_pattern: None,
            last_used: 0,
            usage_count: 1,
            success_rate: 1.0,
            created_from_dag_task_id: None,
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: SkillTrust::Untrusted,
            module_count: 1,
            mutation_history: vec![],
        };
        store.save(&skill).unwrap();

        let params = serde_json::json!({"name": "to-delete"});
        let result = handle_skill_delete(&bb, params);
        assert!(result.is_ok());

        // Soft-delete: skill ainda existe mas usage_count = 0
        let deleted = store.get("to-delete");
        assert!(deleted.is_some());
        assert_eq!(deleted.unwrap().usage_count, 0);
    }
}
