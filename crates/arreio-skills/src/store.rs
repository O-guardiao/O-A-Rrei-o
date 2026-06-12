use anyhow::Result;
use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Registro de uma mutação em uma skill (rastreabilidade completa).
/// Absorvido do BaseStore.mutation_history do Continual Harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationRecord {
    /// Timestamp UNIX da mutação
    pub timestamp: u64,
    /// Quem fez a mutação ("auto-learner", "curator", "refiner", "developer-agent", "human")
    pub source: String,
    /// Campos alterados: nome_do_campo → (valor_antigo, valor_novo)
    pub fields: HashMap<String, MutationChange>,
    /// Motivo da alteração (opcional)
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationChange {
    pub old_value: serde_json::Value,
    pub new_value: serde_json::Value,
}

/// Nível de confiança da skill — derivado da pesquisa de segurança do ecossistema.
/// 26.1% das skills em marketplaces contêm vulnerabilidades.
/// Skills auto-aprendidas começam como Untrusted e precisam passar pelo SkillValidator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SkillTrust {
    /// Auto-aprendida ou importada, ainda não validada. Roda em modo restrito.
    Untrusted,
    /// Passou na validação automática (security scan + contract check + schema check).
    Validated,
    /// Curada manualmente, pronta para produção. Pode usar allowed_tools expandido.
    Trusted,
    /// AST signature não bate com o projeto atual — skill potencialmente obsoleta.
    Stale,
}

impl Default for SkillTrust {
    fn default() -> Self {
        SkillTrust::Untrusted
    }
}

/// Skill aprendida — procedimento reutilizável no estilo Codex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub trigger_patterns: Vec<String>, // regex simples como strings
    pub ast_signature: Option<String>, // JSON compacto do resultado típico
    pub file_target_pattern: Option<String>,
    pub instruction_template: String,
    // NOVOS CAMPOS — extraídos do padrão Codex
    pub steps: Vec<String>,                 // passos ordenados do workflow
    pub templates: HashMap<String, String>, // nome → template de código
    pub validation_cmds: Vec<String>,       // comandos de validação
    pub last_used: u64,                     // timestamp unix
    pub usage_count: u64,
    pub success_rate: f32, // 0.0 – 1.0
    pub created_from_dag_task_id: Option<String>,
    // ── Regras de Ouro & Insights (Harness Pattern + SkillsBench Research) ──
    /// Regra de Ouro #1: Proíbe saída com conversação social (ex: "Olá! Aqui está...").
    /// Quando true, a saída deve ser estritamente JSON estruturado ou erro.
    pub anti_conversation: bool,
    /// Regra de Ouro #2: Mesma entrada → mesma saída computacional.
    /// Essencial para skills com math engine determinístico.
    pub idempotent: bool,
    /// Regra de Ouro #3: Máximo de auto-correções antes de Human-in-the-loop.
    /// Alinhado com o Watchdog (3× exit_code → StrategicRetreat). Default: 3.
    pub error_budget: u32,
    /// Coerção de Gramática: JSON Schema para validação estrita da saída.
    /// Ex: {"type":"object","required":["status","result"],"properties":{...}}
    pub output_schema: Option<String>,
    /// Ferramentas permitidas para esta skill (vazio = todas liberadas).
    /// Ex: ["Read","Grep","Write"] para skills read-write controladas.
    pub allowed_tools: Vec<String>,
    /// Nível de confiança: Untrusted → Validated → Trusted.
    /// Auto-aprendidas nascem Untrusted. Precisam passar SkillValidator.
    pub trust_level: SkillTrust,
    /// Número de módulos focados (SkillsBench: 2-3 é ótimo, 4+ degrada).
    pub module_count: u32,
    /// Histórico de mutações (rastreabilidade completa, append-only).
    /// Cada update_skill() adiciona um registro com diff dos campos alterados.
    #[serde(default)]
    pub mutation_history: Vec<MutationRecord>,
}

/// Persistência de skills no Blackboard (categoria "skills").
pub struct SkillStore {
    blackboard: Blackboard,
}

impl SkillStore {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn save(&self, skill: &Skill) -> Result<()> {
        let value = serde_json::to_value(skill)?;
        self.blackboard.put_tuple("skills", &skill.name, value)
    }

    pub fn get(&self, name: &str) -> Option<Skill> {
        self.blackboard
            .get_tuple("skills", name)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    pub fn list(&self) -> Vec<Skill> {
        self.blackboard
            .search_tuples("skills", "")
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect()
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        // Soft-delete: salva com usage_count = 0 como sinal
        if let Some(mut skill) = self.get(name) {
            skill.usage_count = 0;
            skill.success_rate = 0.0;
            self.save(&skill)?;
        }
        Ok(())
    }

    /// Atualiza uma skill com rastreabilidade de mutação.
    /// Registra automaticamente um MutationRecord com diff dos campos alterados.
    pub fn update_skill(
        &self,
        name: &str,
        source: &str,
        reason: Option<&str>,
        updater: impl FnOnce(&mut Skill),
    ) -> Result<()> {
        let mut skill = self
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Skill '{}' não encontrada", name))?;

        // Captura snapshot pré-mutação
        let old_snapshot = serde_json::to_value(&skill)?;

        // Aplica a mutação
        updater(&mut skill);

        // Captura snapshot pós-mutação
        let new_snapshot = serde_json::to_value(&skill)?;

        // Constrói MutationRecord com diff (apenas campos alterados)
        let mut fields = HashMap::new();
        if let (serde_json::Value::Object(old_map), serde_json::Value::Object(new_map)) =
            (&old_snapshot, &new_snapshot)
        {
            for (key, new_val) in new_map {
                // Não registra meta-campos
                if key == "mutation_history" || key == "last_used" {
                    continue;
                }
                if let Some(old_val) = old_map.get(key) {
                    if old_val != new_val {
                        fields.insert(
                            key.clone(),
                            MutationChange {
                                old_value: old_val.clone(),
                                new_value: new_val.clone(),
                            },
                        );
                    }
                }
            }
        }

        if !fields.is_empty() {
            skill.mutation_history.push(MutationRecord {
                timestamp: now_epoch_secs(),
                source: source.to_string(),
                fields,
                reason: reason.map(String::from),
            });
        }

        self.save(&skill)
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Skill {
    /// Converte para SkillMd (usado pelo Curator).
    pub fn to_skill_md(&self) -> crate::skill_md::SkillMd {
        crate::skill_md::SkillMd {
            name: self.name.clone(),
            description: self.description.clone(),
            version: "1.0".to_string(),
            license: None,
            platforms: vec![],
            prerequisites: vec![],
            tags: self.trigger_patterns.clone(),
            related_skills: vec![],
            when_to_use: String::new(),
            when_not_to_use: String::new(),
            quick_reference: self.instruction_template.clone(),
            examples: String::new(),
            body: String::new(),
            // ── Regras de Ouro & Insights ──
            error_budget: self.error_budget,
            output_schema: self.output_schema.clone(),
            allowed_tools: self.allowed_tools.clone(),
            anti_conversation: self.anti_conversation,
            idempotent: self.idempotent,
            trust_level: format!("{:?}", self.trust_level).to_lowercase(),
            module_count: self.module_count,
        }
    }

    /// Converte para SkillTelemetry (usado pelo Curator).
    pub fn to_telemetry(&self) -> crate::skill_md::SkillTelemetry {
        crate::skill_md::SkillTelemetry {
            use_count: self.usage_count,
            view_count: 0,
            last_used_at: self.last_used,
            patch_count: 0,
            state: if self.usage_count == 0 {
                crate::skill_md::SkillState::Archived
            } else {
                crate::skill_md::SkillState::Active
            },
            pinned: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn skill_store_roundtrip() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let skill = Skill {
            name: "rust-auth".into(),
            description: "Autenticação em Rust".into(),
            trigger_patterns: vec!["auth".into(), "login".into()],
            ast_signature: None,
            file_target_pattern: Some("src/auth.rs".into()),
            instruction_template: "Implemente autenticação JWT".into(),
            steps: vec!["Criar modelo".into(), "Implementar handler".into()],
            templates: Default::default(),
            validation_cmds: vec!["cargo test auth".into()],
            last_used: 0,
            usage_count: 0,
            success_rate: 1.0,
            created_from_dag_task_id: Some("t1".into()),
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: SkillTrust::Trusted,
            module_count: 2,
            mutation_history: vec![],
        };
        store.save(&skill).unwrap();
        let retrieved = store.get("rust-auth").unwrap();
        assert_eq!(retrieved.description, "Autenticação em Rust");
    }

    #[test]
    fn mutation_history_registra_alteracao() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let skill = make_test_skill("test-skill", SkillTrust::Untrusted);
        store.save(&skill).unwrap();

        store
            .update_skill("test-skill", "curator", Some("validação automática"), |s| {
                s.trust_level = SkillTrust::Validated;
                s.description = "Nova descrição".into();
            })
            .unwrap();

        let updated = store.get("test-skill").unwrap();
        assert_eq!(updated.mutation_history.len(), 1);
        let record = &updated.mutation_history[0];
        assert_eq!(record.source, "curator");
        assert!(record.fields.contains_key("trust_level"));
        assert_eq!(updated.trust_level, SkillTrust::Validated);
    }

    #[test]
    fn mutation_history_nao_registra_sem_alteracao() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let skill = make_test_skill("test-skill2", SkillTrust::Untrusted);
        store.save(&skill).unwrap();

        store
            .update_skill("test-skill2", "curator", None, |_s| {
                // Nenhuma alteração real
            })
            .unwrap();

        let updated = store.get("test-skill2").unwrap();
        assert!(updated.mutation_history.is_empty());
    }

    fn make_test_skill(name: &str, trust: SkillTrust) -> Skill {
        Skill {
            name: name.into(),
            description: "Skill de teste".into(),
            trigger_patterns: vec!["test".into()],
            ast_signature: None,
            file_target_pattern: None,
            instruction_template: "execute".into(),
            steps: vec!["Passo 1".into()],
            templates: Default::default(),
            validation_cmds: vec![],
            last_used: 0,
            usage_count: 1,
            success_rate: 1.0,
            created_from_dag_task_id: None,
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: trust,
            module_count: 1,
            mutation_history: vec![],
        }
    }
}
