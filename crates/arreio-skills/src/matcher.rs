use crate::store::{Skill, SkillStore};

/// Encontra skills relevantes para uma spec ou instrução.
pub struct SkillMatcher {
    store: SkillStore,
}

impl SkillMatcher {
    pub fn new(store: SkillStore) -> Self {
        Self { store }
    }

    /// Retorna skills cujos trigger_patterns matcham com a query.
    /// Rankeadas por: usage_count desc, last_used desc, success_rate desc.
    pub fn find_relevant(&self, query: &str) -> Vec<Skill> {
        let query_lower = query.to_lowercase();
        let mut skills: Vec<Skill> = self
            .store
            .list()
            .into_iter()
            .filter(|skill| skill.trust_level != crate::store::SkillTrust::Stale)
            .filter(|skill| skill.usage_count > 0 || skill.success_rate > 0.0)
            .filter(|skill| {
                skill
                    .trigger_patterns
                    .iter()
                    .any(|pattern| query_lower.contains(&pattern.to_lowercase()))
            })
            .collect();

        // Rankeamento: mais usadas + mais recentes primeiro
        skills.sort_by(|a, b| {
            let by_usage = b.usage_count.cmp(&a.usage_count);
            if by_usage != std::cmp::Ordering::Equal {
                return by_usage;
            }
            let by_time = b.last_used.cmp(&a.last_used);
            if by_time != std::cmp::Ordering::Equal {
                return by_time;
            }
            b.success_rate
                .partial_cmp(&a.success_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        skills
    }

    /// Formata skills para injeção no contexto do ator.
    /// Inclui steps, templates e validation commands quando disponíveis.
    /// Invalida skills cujo ast_signature não bate com a signature atual do projeto.
    /// Retorna o número de skills marcadas como Stale.
    pub fn invalidate_stale(&self, current_ast_sig: &str) -> usize {
        let mut count = 0;
        for mut skill in self.store.list() {
            if let Some(ref sig) = skill.ast_signature {
                if sig != current_ast_sig && skill.trust_level != crate::store::SkillTrust::Stale {
                    skill.trust_level = crate::store::SkillTrust::Stale;
                    let _ = self.store.save(&skill);
                    count += 1;
                }
            }
        }
        count
    }

    pub fn format_context(skills: &[Skill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut ctx = "## Skills Relevantes Aprendidas\n\n".to_string();
        for skill in skills {
            ctx.push_str(&format!(
                "- **{}** (usos={}, success={:.0}%): {}\n  Template: `{}`\n",
                skill.name,
                skill.usage_count,
                skill.success_rate * 100.0,
                skill.description,
                skill.instruction_template
            ));
            if !skill.steps.is_empty() {
                ctx.push_str("  Steps:\n");
                for (i, step) in skill.steps.iter().enumerate() {
                    ctx.push_str(&format!("    {}. {}\n", i + 1, step));
                }
            }
            if !skill.templates.is_empty() {
                ctx.push_str("  Templates:\n");
                for (name, tmpl) in &skill.templates {
                    ctx.push_str(&format!(
                        "    - {}: `{}`\n",
                        name,
                        tmpl.chars().take(60).collect::<String>()
                    ));
                }
            }
            if !skill.validation_cmds.is_empty() {
                ctx.push_str(&format!(
                    "  Validation: `{}`\n",
                    skill.validation_cmds.join("; ")
                ));
            }
        }
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_skill(name: &str, patterns: Vec<&str>) -> Skill {
        Skill {
            name: name.into(),
            description: "desc".into(),
            trigger_patterns: patterns.into_iter().map(String::from).collect(),
            ast_signature: None,
            file_target_pattern: None,
            instruction_template: "do it".into(),
            steps: vec![],
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
            trust_level: crate::store::SkillTrust::Trusted,
            module_count: 1,
            mutation_history: vec![],
        }
    }

    #[test]
    fn matcher_encontra_por_trigger() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        store
            .save(&make_skill("auth", vec!["autenticação", "login"]))
            .unwrap();

        let matcher = SkillMatcher::new(store);
        let found = matcher.find_relevant("implementar autenticação JWT");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn matcher_exclui_skills_stale() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let mut skill = make_skill("auth", vec!["autenticação"]);
        skill.trust_level = crate::store::SkillTrust::Stale;
        store.save(&skill).unwrap();

        let matcher = SkillMatcher::new(store);
        let found = matcher.find_relevant("implementar autenticação JWT");
        assert!(found.is_empty(), "skills Stale não devem ser retornadas");
    }

    #[test]
    fn invalidate_stale_marks_mismatched_signatures() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let mut skill = make_skill("auth", vec!["autenticação"]);
        skill.ast_signature = Some("old-sig".to_string());
        store.save(&skill).unwrap();

        let matcher = SkillMatcher::new(store);
        let count = matcher.invalidate_stale("new-sig");
        assert_eq!(count, 1);

        let found = matcher.find_relevant("autenticação");
        assert!(found.is_empty(), "skill com sig mismatch deve estar Stale");
    }
}
