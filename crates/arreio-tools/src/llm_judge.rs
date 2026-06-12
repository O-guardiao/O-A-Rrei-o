//! LlmAsJudge — avaliação de qualidade como tool explícita (PVC-Q2.2).
//!
//! Traduz o padrão "LLM-as-a-Judge" para a arquitetura O Arreio: o juiz é uma
//! tool registrada no ToolRegistry (invocada pelo harness/Planner, nunca um
//! orquestrador), com critérios configuráveis e pesos determinísticos.
//! O score agregado é calculado pelo harness a partir dos scores por critério
//! retornados pelo LLM — o LLM nunca decide o veredito final sozinho.

use crate::{ToolHandler, ToolRequest, ToolResult};
use anyhow::{bail, Context, Result};
use arreio_provider::{ChatRequest, ProviderClient, ToolDescriptor, ToolFunction};
use serde::{Deserialize, Serialize};

/// Critério de julgamento com peso.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeCriterion {
    pub name: String,
    pub description: String,
    pub weight: f64,
}

impl JudgeCriterion {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        weight: f64,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            weight,
        }
    }
}

/// Score atribuído pelo LLM a um critério.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore {
    pub name: String,
    /// Score em [0.0, 1.0] (clampado pelo harness).
    pub score: f64,
    pub rationale: String,
}

/// Veredito consolidado — score agregado calculado deterministicamente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// Média ponderada dos scores por critério, em [0.0, 1.0].
    pub overall_score: f64,
    /// True se overall_score >= pass_threshold.
    pub pass: bool,
    pub criteria: Vec<CriterionScore>,
    pub reasoning: String,
}

/// Resposta crua esperada do LLM (apenas scores por critério).
#[derive(Debug, Deserialize)]
struct RawJudgeResponse {
    criteria: Vec<CriterionScore>,
    #[serde(default)]
    reasoning: String,
}

/// Juiz LLM com critérios configuráveis.
pub struct LlmAsJudge {
    client: Box<dyn ProviderClient>,
    model: String,
    criteria: Vec<JudgeCriterion>,
    pass_threshold: f64,
}

impl LlmAsJudge {
    /// Critérios padrão: corretude (0.5), completude (0.3), clareza (0.2).
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            criteria: vec![
                JudgeCriterion::new(
                    "correctness",
                    "A resposta está tecnicamente correta e atende à tarefa?",
                    0.5,
                ),
                JudgeCriterion::new(
                    "completeness",
                    "A resposta cobre todos os requisitos da tarefa?",
                    0.3,
                ),
                JudgeCriterion::new(
                    "clarity",
                    "A resposta é clara, organizada e sem ambiguidade?",
                    0.2,
                ),
            ],
            pass_threshold: 0.7,
        }
    }

    /// Substitui os critérios padrão.
    pub fn with_criteria(mut self, criteria: Vec<JudgeCriterion>) -> Self {
        self.criteria = criteria;
        self
    }

    pub fn with_pass_threshold(mut self, threshold: f64) -> Self {
        self.pass_threshold = threshold;
        self
    }

    /// Julga um candidato. `reference` é opcional (gabarito).
    /// Falha de parsing retorna Err — nunca inventa score.
    pub fn judge(
        &self,
        task: &str,
        candidate: &str,
        reference: Option<&str>,
    ) -> Result<JudgeVerdict> {
        if self.criteria.is_empty() {
            bail!("LlmAsJudge sem critérios configurados");
        }

        let criteria_desc: String = self
            .criteria
            .iter()
            .map(|c| format!("- {}: {}", c.name, c.description))
            .collect::<Vec<_>>()
            .join("\n");

        let reference_block = reference
            .map(|r| format!("\n\nREFERÊNCIA (gabarito):\n{}", r))
            .unwrap_or_default();

        let user = format!(
            "TAREFA:\n{}\n\nCANDIDATO A AVALIAR:\n{}{}\n\nCRITÉRIOS:\n{}\n\n\
             Avalie cada critério com score entre 0.0 e 1.0. \
             Retorne SOMENTE JSON no formato:\n\
             {{\"criteria\": [{{\"name\": \"...\", \"score\": 0.0, \"rationale\": \"...\"}}], \
             \"reasoning\": \"...\"}}",
            task, candidate, reference_block, criteria_desc
        );

        let req = ChatRequest::new(
            self.model.clone(),
            "Você é um avaliador rigoroso e imparcial. Responda apenas JSON.",
            user,
        );
        let response = self.client.chat(req)?;
        let clean = arreio_actors::extract_json_block(&response.content);
        let raw: RawJudgeResponse = serde_json::from_str(&clean)
            .with_context(|| format!("juiz retornou JSON inválido: {}", clean))?;

        // Agregação determinística no harness: média ponderada dos critérios
        // configurados. Critérios ausentes na resposta contam como 0.
        let mut weighted_sum = 0.0;
        let mut weight_total = 0.0;
        let mut scored = Vec::new();
        for criterion in &self.criteria {
            let entry = raw.criteria.iter().find(|c| c.name == criterion.name);
            let score = entry.map(|c| c.score.clamp(0.0, 1.0)).unwrap_or(0.0);
            weighted_sum += score * criterion.weight;
            weight_total += criterion.weight;
            scored.push(CriterionScore {
                name: criterion.name.clone(),
                score,
                rationale: entry
                    .map(|c| c.rationale.clone())
                    .unwrap_or_else(|| "critério não avaliado pelo LLM".to_string()),
            });
        }
        let overall = if weight_total > 0.0 {
            weighted_sum / weight_total
        } else {
            0.0
        };

        Ok(JudgeVerdict {
            overall_score: overall,
            pass: overall >= self.pass_threshold,
            criteria: scored,
            reasoning: raw.reasoning,
        })
    }

    /// Descriptor para registro no ToolRegistry.
    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "llm_as_judge".to_string(),
                description: "Avalia a qualidade de uma saída candidata contra critérios \
                              configuráveis usando um LLM juiz. Retorna scores por critério, \
                              score agregado e veredito pass/fail."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": {"type": "string", "description": "Descrição da tarefa avaliada"},
                        "candidate": {"type": "string", "description": "Saída candidata a avaliar"},
                        "reference": {"type": "string", "description": "Gabarito opcional"}
                    },
                    "required": ["task", "candidate"]
                }),
            },
        }
    }
}

impl ToolHandler for LlmAsJudge {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let task = request
            .arguments
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let candidate = request
            .arguments
            .get("candidate")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if task.is_empty() || candidate.is_empty() {
            return Ok(ToolResult::err(
                "llm_as_judge requer argumentos 'task' e 'candidate'",
            ));
        }
        let reference = request.arguments.get("reference").and_then(|v| v.as_str());

        match self.judge(task, candidate, reference) {
            Ok(verdict) => Ok(ToolResult::ok(serde_json::to_string(&verdict)?)),
            Err(e) => Ok(ToolResult::err(format!("julgamento falhou: {}", e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_provider::MockProvider;

    fn judge_with_response(response: &str) -> LlmAsJudge {
        LlmAsJudge::new(Box::new(MockProvider::new(response)), "mock")
    }

    #[test]
    fn julga_e_agrega_ponderado() {
        let judge = judge_with_response(
            r#"{"criteria": [
                {"name": "correctness", "score": 1.0, "rationale": "correto"},
                {"name": "completeness", "score": 0.5, "rationale": "parcial"},
                {"name": "clarity", "score": 0.0, "rationale": "confuso"}
            ], "reasoning": "avaliação"}"#,
        );
        let verdict = judge.judge("tarefa", "candidato", None).unwrap();
        // 1.0*0.5 + 0.5*0.3 + 0.0*0.2 = 0.65
        assert!((verdict.overall_score - 0.65).abs() < 1e-9);
        assert!(!verdict.pass); // threshold 0.7
        assert_eq!(verdict.criteria.len(), 3);
    }

    #[test]
    fn passa_quando_acima_do_threshold() {
        let judge = judge_with_response(
            r#"{"criteria": [
                {"name": "correctness", "score": 1.0, "rationale": "ok"},
                {"name": "completeness", "score": 0.9, "rationale": "ok"},
                {"name": "clarity", "score": 0.8, "rationale": "ok"}
            ]}"#,
        );
        let verdict = judge.judge("tarefa", "candidato", None).unwrap();
        assert!(verdict.pass);
    }

    #[test]
    fn criterio_ausente_conta_zero() {
        let judge = judge_with_response(
            r#"{"criteria": [{"name": "correctness", "score": 1.0, "rationale": "ok"}]}"#,
        );
        let verdict = judge.judge("tarefa", "candidato", None).unwrap();
        // Apenas correctness (0.5 de peso): 1.0*0.5 / 1.0 = 0.5
        assert!((verdict.overall_score - 0.5).abs() < 1e-9);
        assert!(verdict
            .criteria
            .iter()
            .any(|c| c.rationale.contains("não avaliado")));
    }

    #[test]
    fn score_fora_do_intervalo_e_clampado() {
        let judge = judge_with_response(
            r#"{"criteria": [
                {"name": "correctness", "score": 7.5, "rationale": "exagerado"},
                {"name": "completeness", "score": -2.0, "rationale": "negativo"},
                {"name": "clarity", "score": 1.0, "rationale": "ok"}
            ]}"#,
        );
        let verdict = judge.judge("tarefa", "candidato", None).unwrap();
        for c in &verdict.criteria {
            assert!((0.0..=1.0).contains(&c.score));
        }
    }

    #[test]
    fn json_invalido_retorna_erro_sem_inventar_score() {
        let judge = judge_with_response("desculpe, não consigo avaliar");
        assert!(judge.judge("tarefa", "candidato", None).is_err());
    }

    #[test]
    fn criterios_customizados() {
        let mock = MockProvider::new(
            r#"{"criteria": [{"name": "seguranca", "score": 1.0, "rationale": "sem falhas"}]}"#,
        );
        let judge = LlmAsJudge::new(Box::new(mock), "mock")
            .with_criteria(vec![JudgeCriterion::new(
                "seguranca",
                "código sem vulnerabilidades",
                1.0,
            )])
            .with_pass_threshold(0.9);
        let verdict = judge.judge("revisar", "código", None).unwrap();
        assert!((verdict.overall_score - 1.0).abs() < 1e-9);
        assert!(verdict.pass);
    }

    #[test]
    fn tool_handler_roundtrip() {
        let judge = judge_with_response(
            r#"{"criteria": [
                {"name": "correctness", "score": 1.0, "rationale": "ok"},
                {"name": "completeness", "score": 1.0, "rationale": "ok"},
                {"name": "clarity", "score": 1.0, "rationale": "ok"}
            ]}"#,
        );
        let result = judge
            .handle(ToolRequest {
                name: "llm_as_judge".into(),
                arguments: serde_json::json!({"task": "t", "candidate": "c"}),
            })
            .unwrap();
        assert!(result.success);
        let verdict: JudgeVerdict = serde_json::from_str(&result.output).unwrap();
        assert!(verdict.pass);
    }

    #[test]
    fn tool_handler_exige_argumentos() {
        let judge = judge_with_response("{}");
        let result = judge
            .handle(ToolRequest {
                name: "llm_as_judge".into(),
                arguments: serde_json::json!({}),
            })
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn descriptor_valido() {
        let d = LlmAsJudge::descriptor();
        assert_eq!(d.function.name, "llm_as_judge");
        assert!(d.function.parameters["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "candidate"));
    }
}
