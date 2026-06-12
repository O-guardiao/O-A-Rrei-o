use crate::skill_md::{SkillMd, SkillState, SkillTelemetry, SkillTelemetrySidecar};
use crate::store::Skill;
use crate::validator::{SkillValidator, ValidationResult};
use std::collections::HashMap;

/// Curador de skills: identifica clusters, cria umbrellas, arquiva skills obsoletas,
/// e promove trust_level via SkillValidator.
/// Hard rules: nunca toca bundled/hub skills; nunca deleta (apenas archive); pinned ignoradas.
pub struct Curator {
    sidecar: SkillTelemetrySidecar,
    bundled_prefixes: Vec<String>,
    validator: SkillValidator,
}

impl Curator {
    pub fn new() -> Self {
        Self {
            sidecar: SkillTelemetrySidecar::new(),
            bundled_prefixes: vec!["builtin-".into(), "hub-".into(), "system-".into()],
            validator: SkillValidator::new(),
        }
    }

    pub fn with_sidecar(mut self, sidecar: SkillTelemetrySidecar) -> Self {
        self.sidecar = sidecar;
        self
    }

    pub fn with_validator(mut self, validator: SkillValidator) -> Self {
        self.validator = validator;
        self
    }

    pub fn add_bundled_prefix(&mut self, prefix: impl Into<String>) {
        self.bundled_prefixes.push(prefix.into());
    }

    /// Verifica se uma skill é bundled/hub.
    pub fn is_bundled(&self, skill: &SkillMd) -> bool {
        self.bundled_prefixes
            .iter()
            .any(|p| skill.name.starts_with(p))
    }

    /// Clusteriza skills por tag comum.
    pub fn find_clusters(&self, skills: &[SkillMd]) -> Vec<Cluster> {
        let mut tag_to_skills: HashMap<String, Vec<String>> = HashMap::new();
        for skill in skills {
            if self.is_bundled(skill) {
                continue;
            }
            for tag in &skill.tags {
                tag_to_skills
                    .entry(tag.clone())
                    .or_default()
                    .push(skill.name.clone());
            }
        }

        tag_to_skills
            .into_iter()
            .filter(|(_, names)| names.len() >= 2)
            .map(|(tag, names)| Cluster { tag, skills: names })
            .collect()
    }

    /// Identifica skills candidatas a umbrella (clusters com 3+ skills).
    pub fn find_umbrella_candidates(&self, skills: &[SkillMd]) -> Vec<UmbrellaProposal> {
        let clusters = self.find_clusters(skills);
        clusters
            .into_iter()
            .filter(|c| c.skills.len() >= 3)
            .map(|c| UmbrellaProposal {
                name: format!("umbrella-{}", c.tag),
                tag: c.tag.clone(),
                covered_skills: c.skills,
            })
            .collect()
    }

    /// Avalia skills para arquivamento.
    pub fn find_archive_candidates(
        &self,
        skills: &[(SkillMd, SkillTelemetry)],
    ) -> Vec<ArchiveCandidate> {
        let mut candidates = Vec::new();
        for (skill, telemetry) in skills {
            if self.is_bundled(skill) || telemetry.pinned {
                continue;
            }
            if telemetry.state == SkillState::Archived {
                candidates.push(ArchiveCandidate {
                    skill_name: skill.name.clone(),
                    reason: "already archived".to_string(),
                });
            } else if telemetry.use_count == 0 && telemetry.view_count == 0 {
                candidates.push(ArchiveCandidate {
                    skill_name: skill.name.clone(),
                    reason: "never used or viewed".to_string(),
                });
            }
        }
        candidates
    }

    /// Executa uma rodada completa de curadoria.
    pub fn run(&self, skills: &[(SkillMd, SkillTelemetry)]) -> CuratorReport {
        let skill_list: Vec<SkillMd> = skills.iter().map(|(s, _)| s.clone()).collect();
        let clusters = self.find_clusters(&skill_list);
        let umbrellas = self.find_umbrella_candidates(&skill_list);
        let archive = self.find_archive_candidates(skills);

        CuratorReport {
            clusters,
            umbrellas,
            archive_candidates: archive,
            validation_results: None,
            codebase_validations: None,
        }
    }

    /// Executa curadoria sobre skills do SkillStore (runtime).
    /// Aplica SkillValidator.promote_if_valid() em cada skill não-bundled.
    pub fn run_on_store_skills(&self, skills: &mut [Skill]) -> CuratorReport {
        // ── Validação e promoção de trust_level ──
        let mut validation_results: Vec<ValidationResult> = Vec::new();
        for skill in skills.iter_mut() {
            // Não valida skills bundled/hub
            let is_bundled = self
                .bundled_prefixes
                .iter()
                .any(|p| skill.name.starts_with(p));
            if !is_bundled {
                let results = self.validator.promote_if_valid(skill);
                validation_results.extend(results);
            }
        }

        let pairs: Vec<(SkillMd, SkillTelemetry)> = skills
            .iter()
            .map(|s| (s.to_skill_md(), s.to_telemetry()))
            .collect();
        let mut report = self.run(&pairs);
        report.validation_results = Some(validation_results);
        report
    }

    /// Verifica se uma skill ainda é válida contra o codebase atual.
    /// Detecta skills obsoletas por mudança de AST, arquivo removido, ou triggers irrelevantes.
    pub fn verify_against_codebase(
        &self,
        skill: &Skill,
        ast: &arreio_ast::SymbolMap,
    ) -> SkillCodebaseValidation {
        let mut reasons = Vec::new();
        let mut valid = true;

        // Coleta todos os símbolos do AST em um vetor de strings
        let mut symbols: Vec<String> = Vec::new();
        for f in &ast.functions {
            symbols.push(f.name.clone());
        }
        for t in &ast.types {
            symbols.push(t.name.clone());
        }
        for i in &ast.imports {
            symbols.push(i.clone());
        }

        // Verifica AST signature
        if let Some(ref sig) = skill.ast_signature {
            let matching = symbols.iter().any(|s| s.contains(sig));
            if !matching {
                valid = false;
                reasons.push("AST signature não encontrada no código atual".to_string());
            }
        }

        // Verifica file_target_pattern
        if let Some(ref pattern) = skill.file_target_pattern {
            let files_exist = std::fs::read_dir(".")
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| e.path().to_string_lossy().contains(pattern))
                })
                .unwrap_or(false);
            if !files_exist {
                valid = false;
                reasons.push(format!("file_target_pattern '{}' não encontrado", pattern));
            }
        }

        // Verifica trigger_patterns
        for trigger in &skill.trigger_patterns {
            let trigger_found = symbols.iter().any(|s| s.contains(trigger))
                || std::fs::read_dir(".")
                    .map(|entries| {
                        entries.filter_map(|e| e.ok()).any(|e| {
                            e.file_name().to_string_lossy().contains(trigger)
                        })
                    })
                    .unwrap_or(false);
            if !trigger_found {
                valid = false;
                reasons.push(format!("trigger_pattern '{}' não encontrado", trigger));
            }
        }

        SkillCodebaseValidation {
            skill_name: skill.name.clone(),
            valid,
            obsolete_reasons: reasons,
            checked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// Verifica todas as skills do store contra o codebase e retorna relatório.
    pub fn verify_all_against_codebase(
        &self,
        skills: &[Skill],
        ast: &arreio_ast::SymbolMap,
    ) -> Vec<SkillCodebaseValidation> {
        skills
            .iter()
            .map(|s| self.verify_against_codebase(s, ast))
            .collect()
    }
}

impl Default for Curator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Cluster {
    pub tag: String,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UmbrellaProposal {
    pub name: String,
    pub tag: String,
    pub covered_skills: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchiveCandidate {
    pub skill_name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CuratorReport {
    pub clusters: Vec<Cluster>,
    pub umbrellas: Vec<UmbrellaProposal>,
    pub archive_candidates: Vec<ArchiveCandidate>,
    /// Resultados da validação via SkillValidator (None se run() foi usado sem store skills).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_results: Option<Vec<ValidationResult>>,
    /// Resultados da verificação contra codebase (None se não verificado).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codebase_validations: Option<Vec<SkillCodebaseValidation>>,
}

/// Resultado da verificação de uma skill contra o codebase atual.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillCodebaseValidation {
    pub skill_name: String,
    pub valid: bool,
    pub obsolete_reasons: Vec<String>,
    pub checked_at: u64,
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(name: &str, tags: &[&str]) -> SkillMd {
        SkillMd {
            name: name.to_string(),
            description: format!("Skill {}", name),
            version: "1.0".to_string(),
            license: None,
            platforms: vec![],
            prerequisites: vec![],
            tags: tags.iter().map(|t| t.to_string()).collect(),
            related_skills: vec![],
            when_to_use: String::new(),
            when_not_to_use: String::new(),
            quick_reference: String::new(),
            examples: String::new(),
            body: String::new(),
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            anti_conversation: true,
            idempotent: false,
            trust_level: "trusted".to_string(),
            module_count: 1,
        }
    }

    #[test]
    fn curator_detects_bundled() {
        let curator = Curator::new();
        let bundled = make_skill("builtin-auth", &["auth"]);
        let custom = make_skill("my-auth", &["auth"]);
        assert!(curator.is_bundled(&bundled));
        assert!(!curator.is_bundled(&custom));
    }

    #[test]
    fn find_clusters_by_tag() {
        let curator = Curator::new();
        let skills = vec![
            make_skill("s1", &["rust", "api"]),
            make_skill("s2", &["rust", "db"]),
            make_skill("s3", &["rust", "auth"]),
            make_skill("s4", &["python", "api"]),
        ];
        let clusters = curator.find_clusters(&skills);
        let rust_cluster = clusters.iter().find(|c| c.tag == "rust").unwrap();
        assert_eq!(rust_cluster.skills.len(), 3);
        let api_cluster = clusters.iter().find(|c| c.tag == "api").unwrap();
        assert_eq!(api_cluster.skills.len(), 2);
    }

    #[test]
    fn find_umbrella_candidates() {
        let curator = Curator::new();
        let skills = vec![
            make_skill("s1", &["rust"]),
            make_skill("s2", &["rust"]),
            make_skill("s3", &["rust"]),
            make_skill("s4", &["rust"]),
        ];
        let umbrellas = curator.find_umbrella_candidates(&skills);
        assert_eq!(umbrellas.len(), 1);
        assert_eq!(umbrellas[0].tag, "rust");
        assert_eq!(umbrellas[0].covered_skills.len(), 4);
    }

    #[test]
    fn find_archive_candidates() {
        let curator = Curator::new();
        let skills = vec![
            (
                make_skill("unused", &["x"]),
                SkillTelemetry {
                    use_count: 0,
                    view_count: 0,
                    last_used_at: 0,
                    patch_count: 0,
                    state: SkillState::Active,
                    pinned: false,
                },
            ),
            (
                make_skill("used", &["x"]),
                SkillTelemetry {
                    use_count: 5,
                    view_count: 10,
                    last_used_at: 999999,
                    patch_count: 0,
                    state: SkillState::Active,
                    pinned: false,
                },
            ),
        ];
        let candidates = curator.find_archive_candidates(&skills);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].skill_name, "unused");
    }

    #[test]
    fn pinned_skills_not_archived() {
        let curator = Curator::new();
        let skills = vec![(
            make_skill("pinned-unused", &["x"]),
            SkillTelemetry {
                use_count: 0,
                view_count: 0,
                last_used_at: 0,
                patch_count: 0,
                state: SkillState::Active,
                pinned: true,
            },
        )];
        let candidates = curator.find_archive_candidates(&skills);
        assert!(candidates.is_empty());
    }

    #[test]
    fn run_generates_full_report() {
        let curator = Curator::new();
        let skills = vec![
            (
                make_skill("s1", &["rust"]),
                SkillTelemetry {
                    use_count: 1,
                    view_count: 1,
                    last_used_at: 999999,
                    patch_count: 0,
                    state: SkillState::Active,
                    pinned: false,
                },
            ),
            (
                make_skill("s2", &["rust"]),
                SkillTelemetry {
                    use_count: 1,
                    view_count: 1,
                    last_used_at: 999999,
                    patch_count: 0,
                    state: SkillState::Active,
                    pinned: false,
                },
            ),
            (
                make_skill("s3", &["rust"]),
                SkillTelemetry {
                    use_count: 1,
                    view_count: 1,
                    last_used_at: 999999,
                    patch_count: 0,
                    state: SkillState::Active,
                    pinned: false,
                },
            ),
            (
                make_skill("unused", &["rust"]),
                SkillTelemetry {
                    use_count: 0,
                    view_count: 0,
                    last_used_at: 0,
                    patch_count: 0,
                    state: SkillState::Active,
                    pinned: false,
                },
            ),
        ];
        let report = curator.run(&skills);
        assert!(!report.clusters.is_empty());
        assert!(!report.umbrellas.is_empty());
        assert!(!report.archive_candidates.is_empty());
    }

    #[test]
    fn run_on_store_skills_valida_e_promove() {
        let curator = Curator::new();
        let mut skills = vec![
            crate::store::Skill {
                name: "untrusted-skill".into(),
                description: "Skill recém-aprendida para teste de validação".into(),
                trigger_patterns: vec!["test".into(), "validate".into()],
                ast_signature: None,
                file_target_pattern: None,
                instruction_template: "Execute o procedimento padrão".into(),
                steps: vec!["Passo 1".into(), "Passo 2".into()],
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
                trust_level: crate::store::SkillTrust::Untrusted,
                module_count: 2,
                mutation_history: vec![],
            },
        ];
        let report = curator.run_on_store_skills(&mut skills);

        // Deve ter resultados de validação
        assert!(report.validation_results.is_some());
        let vr = report.validation_results.unwrap();
        assert!(!vr.is_empty());

        // Skill limpa deve ser promovida para Validated
        assert_eq!(skills[0].trust_level, crate::store::SkillTrust::Validated);
    }
}
