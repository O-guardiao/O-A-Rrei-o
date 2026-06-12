use crate::store::{Skill, SkillStore, SkillTrust};
use anyhow::Result;
use arreio_kernel::Blackboard;
use std::collections::HashMap;

// ── Constantes de cadência adaptativa (inspirado no Continual Harness) ──
const MIN_WARMUP_TASKS: u32 = 10;
const EARLY_PHASE_CUTOFF: u32 = 50;
const EARLY_FREQUENCY: u32 = 5;
const STABLE_FREQUENCY: u32 = 25;

/// Extrai skills automaticamente de tarefas DAG bem-sucedidas.
/// Usa cadência adaptativa: frequente no início (cada 5 tarefas),
/// espaçada depois (cada 25 tarefas), com warmup de 10 tarefas.
pub struct AutoLearner {
    store: SkillStore,
    blackboard: Blackboard,
}

impl AutoLearner {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            store: SkillStore::new(blackboard.clone()),
            blackboard,
        }
    }

    /// Contador de tarefas processadas (persistido no Blackboard).
    fn task_count(&self) -> u32 {
        self.blackboard
            .get_tuple("autolearner", "task_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32
    }

    fn increment_task_count(&self) {
        let count = self.task_count() + 1;
        let _ = self.blackboard.put_tuple(
            "autolearner",
            "task_count",
            serde_json::json!(count),
        );
    }

    /// Decide se deve aprender nesta iteração (cadência adaptativa).
    pub fn should_learn(&self) -> bool {
        let count = self.task_count();
        if count < MIN_WARMUP_TASKS {
            return false;
        }
        let frequency = if count <= EARLY_PHASE_CUTOFF {
            EARLY_FREQUENCY
        } else {
            STABLE_FREQUENCY
        };
        count % frequency == 0
    }

    /// Wrapper que aplica cadência adaptativa antes de aprender.
    /// Incrementa o contador de tarefas e só chama learn_from_task se should_learn().
    pub fn learn_from_task_if_due(
        &self,
        task_id: &str,
        instruction: &str,
        file_target: Option<&str>,
        generated_code: Option<&str>,
        validation_cmd: Option<&str>,
        steps: Vec<String>,
    ) -> Result<Option<Skill>> {
        self.increment_task_count();
        if !self.should_learn() {
            return Ok(None);
        }
        self.learn_from_task(task_id, instruction, file_target, generated_code, validation_cmd, steps)
    }

    /// Tenta criar ou atualizar uma skill a partir de uma tarefa concluída.
    /// Se já existe skill similar, incrementa usage_count em vez de duplicar.
    pub fn learn_from_task(
        &self,
        task_id: &str,
        instruction: &str,
        file_target: Option<&str>,
        generated_code: Option<&str>,
        validation_cmd: Option<&str>,
        steps: Vec<String>,
    ) -> Result<Option<Skill>> {
        let keywords: Vec<String> = instruction
            .split_whitespace()
            .filter(|w| w.len() > 4)
            .take(3)
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_string()
            })
            .collect();

        if keywords.is_empty() {
            return Ok(None);
        }

        let now = now();

        // Verifica se já existe skill similar — se sim, atualiza
        let existing = self.store.list();
        for mut sk in existing {
            let overlap = sk
                .trigger_patterns
                .iter()
                .filter(|p| keywords.iter().any(|k| p.contains(k)))
                .count();
            if overlap > 0 {
                sk.usage_count += 1;
                sk.last_used = now;
                sk.success_rate = ((sk.success_rate * (sk.usage_count as f32 - 1.0)) + 1.0)
                    / sk.usage_count as f32;
                if let Some(cmd) = validation_cmd {
                    if !sk.validation_cmds.contains(&cmd.to_string()) {
                        sk.validation_cmds.push(cmd.to_string());
                    }
                }
                if !steps.is_empty() && sk.steps.is_empty() {
                    sk.steps = steps;
                }
                self.store.save(&sk)?;
                return Ok(Some(sk));
            }
        }

        let name = format!("auto-{}", keywords.join("-"));
        let ast_sig = generated_code.map(|code| {
            let first = code.lines().next().unwrap_or("").to_string();
            format!("{}::lines={}", first, code.lines().count())
        });

        let mut templates = HashMap::new();
        if let Some(code) = generated_code {
            templates.insert("default".into(), code.chars().take(500).collect());
        }

        let mut validation_cmds = Vec::new();
        if let Some(cmd) = validation_cmd {
            validation_cmds.push(cmd.to_string());
        }

        let skill = Skill {
            name: name.clone(),
            description: format!("Skill aprendida automaticamente da tarefa {}", task_id),
            trigger_patterns: keywords,
            ast_signature: ast_sig,
            file_target_pattern: file_target.map(String::from),
            instruction_template: instruction.into(),
            steps,
            templates,
            validation_cmds,
            last_used: now,
            usage_count: 1,
            success_rate: 1.0,
            created_from_dag_task_id: Some(task_id.into()),
            // ── Auto-aprendidas: seguras por padrão, precisam de validação ──
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: SkillTrust::Untrusted,
            module_count: 1,
            mutation_history: vec![],
        };

        self.store.save(&skill)?;
        Ok(Some(skill))
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    fn learn_cria_skill_nova() {
        let bb = temp_bb();
        let learner = AutoLearner::new(bb);
        let skill = learner
            .learn_from_task(
                "t1",
                "implementar autenticação JWT no módulo de login",
                Some("src/auth.rs"),
                Some("fn login() {}"),
                Some("cargo test auth"),
                vec!["Criar modelo".into(), "Implementar handler".into()],
            )
            .unwrap();
        assert!(skill.is_some());
        let sk = skill.unwrap();
        assert!(sk.name.starts_with("auto-"));
        assert_eq!(sk.validation_cmds.len(), 1);
        assert_eq!(sk.steps.len(), 2);
    }

    #[test]
    fn learn_atualiza_em_vez_de_duplicar() {
        let bb = temp_bb();
        let learner = AutoLearner::new(bb.clone());
        let first = learner
            .learn_from_task(
                "t1",
                "implementar autenticação JWT",
                Some("src/auth.rs"),
                None,
                None,
                vec![],
            )
            .unwrap();
        assert!(first.is_some());
        assert_eq!(first.unwrap().usage_count, 1);

        let second = learner
            .learn_from_task(
                "t2",
                "implementar autenticação OAuth",
                Some("src/auth.rs"),
                None,
                Some("cargo test oauth"),
                vec![],
            )
            .unwrap();
        // autenticação já existe → atualiza usage_count em vez de criar nova
        assert!(second.is_some());
        assert_eq!(second.unwrap().usage_count, 2);
    }

    // ── Adaptive Cadence Tests ──────────────────────────────────────

    #[test]
    fn adaptive_cadence_respeita_warmup() {
        let bb = temp_bb();
        let learner = AutoLearner::new(bb);
        // task_count = 0 por padrão — abaixo de MIN_WARMUP_TASKS (10)
        assert!(!learner.should_learn());
    }

    #[test]
    fn adaptive_cadence_frequente_no_inicio() {
        let bb = temp_bb();
        let learner = AutoLearner::new(bb);
        // Simula 15 tarefas processadas
        for _ in 0..14 {
            learner.increment_task_count();
        }
        // task_count = 14, 14 % 5 != 0
        assert!(!learner.should_learn());
        learner.increment_task_count(); // task_count = 15
        assert!(learner.should_learn()); // 15 % 5 == 0
    }

    #[test]
    fn adaptive_cadence_espacada_depois() {
        let bb = temp_bb();
        let learner = AutoLearner::new(bb);
        // Simula 75 tarefas (já passou do cutoff de 50)
        for _ in 0..74 {
            learner.increment_task_count();
        }
        assert!(!learner.should_learn()); // 74 % 25 != 0
        learner.increment_task_count(); // task_count = 75
        assert!(learner.should_learn()); // 75 % 25 == 0
    }

    #[test]
    fn learn_from_task_if_due_pula_quando_nao_deve() {
        let bb = temp_bb();
        let learner = AutoLearner::new(bb);
        // task_count inicial = 0, warmup — não deve aprender
        let result = learner
            .learn_from_task_if_due(
                "t1",
                "implementar algo",
                None,
                None,
                None,
                vec![],
            )
            .unwrap();
        assert!(result.is_none(), "Warmup: não deve aprender");
    }
}
