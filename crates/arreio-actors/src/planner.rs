use anyhow::Result;
use arreio_dag::Contract;
use arreio_provider::{ChatRequest, ProviderClient, SnapshotCache, SystemPromptBuilder};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

/// Milestone de um plano — unidade verificável de trabalho.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub validation_cmd: Option<String>,
    pub decision_notes: Vec<String>,
}

/// Plano estruturado gerado antes da execução.
/// Inspirado no `/plan` do Codex: espec congelada → milestones → execução.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub goal: String,
    pub non_goals: Vec<String>,
    pub constraints: Vec<String>,
    pub milestones: Vec<Milestone>,
    /// Contratos DAC (Deterministic Agent Contracts) aplicáveis a este plano.
    /// Opcional — plans sem contracts funcionam como antes.
    #[serde(default)]
    pub contracts: Vec<Contract>,
}

const PLANNER_SYSTEM: &str = "\
Você é um Planejador de Projeto. Receba uma especificação e retorne SOMENTE um JSON \
com o seguinte formato:\n\
{\n\
  \"goal\": \"string\",\n\
  \"non_goals\": [\"string\"],\n\
  \"constraints\": [\"string\"],\n\
  \"milestones\": [\n\
    {\n\
      \"id\": \"m1\",\n\
      \"title\": \"string\",\n\
      \"description\": \"string\",\n\
      \"acceptance_criteria\": [\"string\"],\n\
      \"validation_cmd\": \"string|null\",\n\
      \"decision_notes\": [\"string\"]\n\
    }\n\
  ]\n\
}\n\
Cada milestone deve ser pequeno o suficiente para ser concluído em um ciclo. \
Inclua comandos de validação quando possível (ex: 'cargo test', 'npm test'). \
Zero explicações fora do JSON.";

/// Ator Planejador — gera um Plan estruturado a partir de uma spec.
pub struct Planner {
    client: Box<dyn ProviderClient>,
    model: String,
    cache: RefCell<SnapshotCache>,
}

impl Planner {
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            cache: RefCell::new(SnapshotCache::new()),
        }
    }

    /// Recebe spec em texto, retorna Plan estruturado.
    pub fn plan(&self, spec: &str) -> Result<Plan> {
        self.plan_with_contracts(spec, vec![])
    }

    /// Recebe spec + contracts predefinidos, retorna Plan estruturado.
    pub fn plan_with_contracts(&self, spec: &str, contracts: Vec<Contract>) -> Result<Plan> {
        let system =
            SystemPromptBuilder::new(&mut self.cache.borrow_mut(), "planner", PLANNER_SYSTEM)
                .model(&self.model)
                .dynamic(spec)
                .build();
        let req = ChatRequest {
            messages: Vec::new(),
            model: self.model.clone(),
            system,
            user: spec.into(),
            tools: None,
        };
        let response = self.client.chat(req)?;
        let clean = crate::extract_json_block(&response.content);
        let mut plan: Plan = serde_json::from_str(&clean).map_err(|e| {
            anyhow::anyhow!("Planejador retornou JSON inválido: {}\n---\n{}", e, clean)
        })?;
        plan.contracts = contracts;
        Ok(plan)
    }
}

/// Deriva um vetor de DagTask a partir de um Plan.
/// Cada milestone vira um nó do DAG (sequencial por padrão).
pub fn plan_to_dag_tasks(plan: &Plan) -> Vec<crate::DagTask> {
    let mut tasks = Vec::new();
    let mut prev_id: Option<String> = None;

    for (i, ms) in plan.milestones.iter().enumerate() {
        let id = if ms.id.is_empty() {
            format!("m{}", i + 1)
        } else {
            ms.id.clone()
        };
        let mut deps = Vec::new();
        if let Some(ref p) = prev_id {
            deps.push(p.clone());
        }
        // Heurística de actor_type: tarefas de exploração/pesquisa vão para subagente explore
        let actor_type = if ms.description.to_lowercase().contains("explore")
            || ms.description.to_lowercase().contains("research")
            || ms.description.to_lowercase().contains("investigate")
            || ms.description.to_lowercase().contains("analyze")
            || ms.title.to_lowercase().contains("explore")
            || ms.title.to_lowercase().contains("research")
            || ms.title.to_lowercase().contains("investigate")
            || ms.title.to_lowercase().contains("analyze")
        {
            "explore"
        } else {
            "developer"
        };
        tasks.push(crate::DagTask {
            id: id.clone(),
            title: ms.title.clone(),
            depends_on: deps,
            actor_type: actor_type.into(),
            file_target: None, // será determinado pelo Developer
            instruction: format!(
                "{}\n\nCritérios de aceite:\n{}",
                ms.description,
                ms.acceptance_criteria.join("\n")
            ),
            // Propaga contracts do plano para tasks que tenham o mesmo milestone id
            contracts: plan.contracts.iter().filter(|c| c.id == id).map(|c| c.id.clone()).collect(),
        });
        prev_id = Some(tasks.last().unwrap().id.clone());
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_to_dag_tasks_sequential() {
        let plan = Plan {
            goal: "Build API".into(),
            non_goals: vec![],
            constraints: vec![],
            contracts: vec![],
            milestones: vec![
                Milestone {
                    id: "m1".into(),
                    title: "Setup".into(),
                    description: "Init project".into(),
                    acceptance_criteria: vec!["cargo check passes".into()],
                    validation_cmd: Some("cargo check".into()),
                    decision_notes: vec![],
                },
                Milestone {
                    id: "m2".into(),
                    title: "Auth".into(),
                    description: "Add auth".into(),
                    acceptance_criteria: vec!["tests pass".into()],
                    validation_cmd: Some("cargo test auth".into()),
                    decision_notes: vec![],
                },
            ],
        };
        let tasks = plan_to_dag_tasks(&plan);
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].depends_on.is_empty());
        assert_eq!(tasks[1].depends_on, vec!["m1"]);
    }

    #[test]
    fn plan_serialize_roundtrip() {
        let plan = Plan {
            goal: "g".into(),
            non_goals: vec!["ng".into()],
            constraints: vec!["c".into()],
            contracts: vec![],
            milestones: vec![Milestone {
                id: "m1".into(),
                title: "t".into(),
                description: "d".into(),
                acceptance_criteria: vec!["a".into()],
                validation_cmd: None,
                decision_notes: vec![],
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.milestones.len(), 1);
    }

    #[test]
    fn plan_backward_compat_no_contracts_field() {
        // Simula JSON antigo (antes de PVC-Q1.1) sem campo contracts
        let json_old = r#"{
            "goal": "legacy",
            "non_goals": [],
            "constraints": [],
            "milestones": []
        }"#;
        let plan: Plan = serde_json::from_str(json_old).unwrap();
        assert!(plan.contracts.is_empty());
    }
}
