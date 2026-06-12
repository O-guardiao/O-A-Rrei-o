//! ReasoningService — raciocínio como serviço auditável (PVC-Q2.1).
//!
//! O serviço executa CoT/ToT/ReAct/PAL sob controle TOTAL do harness:
//! - o modo é escolhido deterministicamente pelo chamador (`PromptMode`);
//! - cada chamada ao LLM passa por verificação de budget ANTES de ocorrer;
//! - cada passo vira tupla auditável com hash encadeado (`ReasoningLedger`);
//! - no modo ReAct, as transições são estados FSM explícitos, nunca um loop
//!   interno do agente;
//! - ações propostas pelo LLM são executadas pelo `ActionExecutor` fornecido
//!   pelo chamador (tipicamente o ToolRegistry sob policy) — nunca livremente.

use crate::budget::{BudgetVerdict, ReasoningBudget};
use crate::ledger::{ReasoningLedger, ReasoningPhase};
use anyhow::{bail, Result};
use arreio_fsm::{AgentState, Fsm};
use arreio_kernel::Blackboard;
use arreio_provider::{ChatRequest, PromptMode, ProviderClient};
use serde_json::Value;

// ── Executor de ações (ReAct / PAL) ───────────────────────────────────────────

/// Executa uma ação proposta pelo LLM. Implementado pelo chamador
/// (ex.: adapter sobre o ToolRegistry com ToolPolicyPipeline).
pub trait ActionExecutor {
    fn execute(&self, tool: &str, args: &Value) -> Result<String>;
}

/// Executor que nega qualquer ação — default seguro para modos sem ação.
pub struct DenyAllExecutor;

impl ActionExecutor for DenyAllExecutor {
    fn execute(&self, tool: &str, _args: &Value) -> Result<String> {
        bail!("nenhum executor de ações configurado (ação '{}' negada)", tool)
    }
}

impl<F> ActionExecutor for F
where
    F: Fn(&str, &Value) -> Result<String>,
{
    fn execute(&self, tool: &str, args: &Value) -> Result<String> {
        self(tool, args)
    }
}

// ── Requisição e resultado ────────────────────────────────────────────────────

/// Requisição de raciocínio — stateless, tudo explícito.
#[derive(Debug, Clone)]
pub struct ReasoningRequest {
    /// Identificador da sessão de raciocínio (chave do ledger).
    pub session_id: String,
    /// Objetivo a raciocinar.
    pub goal: String,
    /// Contexto adicional curado pelo chamador (pode ser vazio).
    pub context: String,
    pub mode: PromptMode,
    pub model: String,
    pub budget: ReasoningBudget,
    /// Número de ramos para ToT (None = default do modo).
    pub branches: Option<usize>,
}

/// Resultado consolidado do raciocínio.
#[derive(Debug, Clone)]
pub struct ReasoningOutcome {
    pub final_answer: String,
    pub steps_recorded: u32,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    /// Some(motivo) se o raciocínio parou por budget.
    pub budget_exceeded: Option<String>,
    /// Integridade da cadeia de hashes ao final.
    pub chain_valid: bool,
    /// Programa gerado (apenas modo ProgramAided) — execução delegada
    /// ao hypervisor pelo chamador.
    pub program: Option<String>,
}

// ── Parsing determinístico dos scaffolds ──────────────────────────────────────

/// Extrai o texto após um marcador de linha (ex.: "ANSWER:").
fn extract_marker(content: &str, marker: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix(marker)
            .map(|rest| rest.trim().to_string())
    })
}

/// Extrai texto multi-linha após o marcador até o fim do conteúdo.
fn extract_marker_multiline(content: &str, marker: &str) -> Option<String> {
    let idx = content.find(marker)?;
    Some(content[idx + marker.len()..].trim().to_string())
}

/// Diretiva produzida pelo LLM em um turno ReAct.
#[derive(Debug, Clone, PartialEq)]
enum ReactDirective {
    Action { tool: String, args: Value },
    Final(String),
    Malformed(String),
}

/// Faz o parsing de uma resposta ReAct: THOUGHT + (ACTION | FINAL).
fn parse_react(content: &str) -> (String, ReactDirective) {
    let thought = extract_marker(content, "THOUGHT:").unwrap_or_default();

    if let Some(final_answer) = extract_marker_multiline(content, "FINAL:") {
        return (thought, ReactDirective::Final(final_answer));
    }

    if let Some(action_raw) = extract_marker_multiline(content, "ACTION:") {
        // Tolera texto após o JSON: corta no último '}' do conteúdo.
        let json_slice = match (action_raw.find('{'), action_raw.rfind('}')) {
            (Some(start), Some(end)) if end > start => &action_raw[start..=end],
            _ => action_raw.as_str(),
        };
        match serde_json::from_str::<Value>(json_slice) {
            Ok(parsed) => {
                let tool = parsed
                    .get("tool")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
                if tool.is_empty() {
                    return (
                        thought,
                        ReactDirective::Malformed("ACTION sem campo 'tool'".into()),
                    );
                }
                let args = parsed.get("args").cloned().unwrap_or(Value::Null);
                return (thought, ReactDirective::Action { tool, args });
            }
            Err(e) => {
                return (
                    thought,
                    ReactDirective::Malformed(format!("ACTION com JSON inválido: {}", e)),
                );
            }
        }
    }

    (
        thought,
        ReactDirective::Malformed("resposta sem ACTION nem FINAL".into()),
    )
}

/// Extrai bloco ```program ... ``` (modo ProgramAided).
fn extract_program_block(content: &str) -> Option<String> {
    let start_marker = "```program";
    let start = content.find(start_marker)? + start_marker.len();
    let rest = &content[start..];
    let end = rest.find("```")?;
    Some(rest[..end].trim().to_string())
}

// ── Serviço ───────────────────────────────────────────────────────────────────

/// Serviço de raciocínio auditável. Stateless: todo estado vive no Blackboard.
pub struct ReasoningService<'a> {
    client: &'a dyn ProviderClient,
    /// FSM opcional: quando presente, o ciclo ReAct dirige os estados
    /// explícitos ReasoningThought/Action/Observation (PVC-Q2.1).
    fsm: Option<&'a Fsm>,
}

impl<'a> ReasoningService<'a> {
    pub fn new(client: &'a dyn ProviderClient) -> Self {
        Self { client, fsm: None }
    }

    /// Conecta a FSM — as transições ReAct passam a ser validadas por ela.
    pub fn with_fsm(mut self, fsm: &'a Fsm) -> Self {
        self.fsm = Some(fsm);
        self
    }

    fn fsm_transition(&self, to: AgentState) -> Result<()> {
        if let Some(fsm) = self.fsm {
            fsm.transition(to)?;
        }
        Ok(())
    }

    /// Executa uma chamada LLM, contabilizando tokens e custo no budget.
    fn call_llm(
        &self,
        req: &ReasoningRequest,
        budget: &mut ReasoningBudget,
        user: &str,
    ) -> Result<(String, u64, u64, f64)> {
        let system = format!(
            "Você é um motor de raciocínio sob controle externo.\n{}",
            req.mode.system_scaffold()
        );
        let chat = ChatRequest::new(req.model.clone(), system, user.to_string());
        let resp = self.client.chat(chat)?;
        let cost = self
            .client
            .cost_estimate(resp.tokens_in as u32, resp.tokens_out as u32);
        budget.record_usage(resp.tokens_in + resp.tokens_out, cost);
        Ok((resp.content, resp.tokens_in, resp.tokens_out, cost))
    }

    /// Ponto de entrada único: despacha conforme o PromptMode.
    pub fn run(
        &self,
        blackboard: &Blackboard,
        req: ReasoningRequest,
        executor: &dyn ActionExecutor,
    ) -> Result<ReasoningOutcome> {
        let mut budget = req.budget.clone();
        budget.start();
        let mut ledger = ReasoningLedger::open(blackboard.clone(), &req.session_id);

        let outcome = match req.mode {
            PromptMode::Direct | PromptMode::ChainOfThought => {
                self.run_single_shot(&req, &mut budget, &mut ledger)
            }
            PromptMode::TreeOfThoughts => self.run_tree(&req, &mut budget, &mut ledger),
            PromptMode::ReActHarnessed => {
                self.run_react(&req, &mut budget, &mut ledger, executor)
            }
            PromptMode::ProgramAided => self.run_program_aided(&req, &mut budget, &mut ledger),
        }?;

        // Persiste o budget consumido para auditoria da sessão.
        blackboard.put_tuple(
            "reasoning",
            &format!("budget:{}", req.session_id),
            serde_json::to_value(&budget)?,
        )?;

        Ok(ReasoningOutcome {
            chain_valid: ledger.verify_chain()?,
            steps_recorded: ledger.len() as u32,
            total_tokens: budget.tokens_used,
            total_cost_usd: budget.cost_used_usd,
            ..outcome
        })
    }

    /// Direct e CoT: uma única chamada, resposta extraída por marcador.
    fn run_single_shot(
        &self,
        req: &ReasoningRequest,
        budget: &mut ReasoningBudget,
        ledger: &mut ReasoningLedger,
    ) -> Result<ReasoningOutcome> {
        if let BudgetVerdict::Exceeded(reason) = budget.check() {
            return Ok(Self::exhausted_outcome(reason.to_string()));
        }
        budget.consume_step();

        let user = format!("{}\n\nContexto:\n{}", req.goal, req.context);
        let (content, tin, tout, cost) = self.call_llm(req, budget, &user)?;

        let answer = extract_marker_multiline(&content, "ANSWER:")
            .unwrap_or_else(|| content.trim().to_string());

        if req.mode == PromptMode::ChainOfThought {
            // CoT: registra a cadeia completa como Thought auditável.
            ledger.append(
                req.mode.as_str(),
                ReasoningPhase::Thought,
                &user,
                &content,
                tin,
                tout,
                cost,
            )?;
            ledger.append(req.mode.as_str(), ReasoningPhase::Final, "", &answer, 0, 0, 0.0)?;
        } else {
            ledger.append(
                req.mode.as_str(),
                ReasoningPhase::Final,
                &user,
                &answer,
                tin,
                tout,
                cost,
            )?;
        }

        Ok(Self::ok_outcome(answer))
    }

    /// ToT harnessed: N ramos em chamadas separadas + seleção determinística
    /// pelo harness (maior SCORE; empate → menor índice).
    fn run_tree(
        &self,
        req: &ReasoningRequest,
        budget: &mut ReasoningBudget,
        ledger: &mut ReasoningLedger,
    ) -> Result<ReasoningOutcome> {
        let n = req.branches.unwrap_or_else(|| req.mode.default_branches());
        let mut best: Option<(usize, f64, String)> = None;

        for i in 0..n {
            if let BudgetVerdict::Exceeded(reason) = budget.check() {
                // Ramos já gerados continuam válidos; seleção ocorre com o que há.
                if best.is_none() {
                    return Ok(Self::exhausted_outcome(reason.to_string()));
                }
                break;
            }
            budget.consume_step();

            let user = format!(
                "{}\n\nContexto:\n{}\n\n(Ramo {} de {} — proponha uma linha independente.)",
                req.goal,
                req.context,
                i + 1,
                n
            );
            let (content, tin, tout, cost) = self.call_llm(req, budget, &user)?;

            let score = extract_marker(&content, "SCORE:")
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let answer = extract_marker_multiline(&content, "ANSWER:")
                .unwrap_or_else(|| content.trim().to_string());

            ledger.append(
                req.mode.as_str(),
                ReasoningPhase::Branch,
                &user,
                &content,
                tin,
                tout,
                cost,
            )?;

            // Seleção determinística: score estritamente maior vence;
            // empate mantém o ramo de menor índice.
            let is_better = best.as_ref().map(|(_, s, _)| score > *s).unwrap_or(true);
            if is_better {
                best = Some((i, score, answer));
            }
        }

        let (idx, score, answer) =
            best.ok_or_else(|| anyhow::anyhow!("ToT não produziu nenhum ramo"))?;
        ledger.append(
            req.mode.as_str(),
            ReasoningPhase::Selection,
            &format!("seleção determinística sobre {} ramos", n),
            &format!("ramo {} selecionado (score {:.3})", idx + 1, score),
            0,
            0,
            0.0,
        )?;
        ledger.append(req.mode.as_str(), ReasoningPhase::Final, "", &answer, 0, 0, 0.0)?;

        Ok(Self::ok_outcome(answer))
    }

    /// ReAct harnessed: ciclos Thought→Action→Observation como estados FSM.
    /// O LLM apenas propõe; o harness executa e decide continuar.
    fn run_react(
        &self,
        req: &ReasoningRequest,
        budget: &mut ReasoningBudget,
        ledger: &mut ReasoningLedger,
        executor: &dyn ActionExecutor,
    ) -> Result<ReasoningOutcome> {
        let mut observations: Vec<String> = Vec::new();

        loop {
            if let BudgetVerdict::Exceeded(reason) = budget.check() {
                self.fsm_transition(AgentState::StrategicRetreat).ok();
                return Ok(Self::exhausted_outcome(reason.to_string()));
            }
            budget.consume_step();
            self.fsm_transition(AgentState::ReasoningThought)?;

            // Contexto reconstruído a cada turno (stateless): goal + observações.
            let mut user = format!("{}\n\nContexto:\n{}", req.goal, req.context);
            for (i, obs) in observations.iter().enumerate() {
                user.push_str(&format!("\n\nOBSERVATION {}: {}", i + 1, obs));
            }

            let (content, tin, tout, cost) = self.call_llm(req, budget, &user)?;
            let (thought, directive) = parse_react(&content);

            ledger.append(
                req.mode.as_str(),
                ReasoningPhase::Thought,
                &user,
                &thought,
                tin,
                tout,
                cost,
            )?;

            match directive {
                ReactDirective::Final(answer) => {
                    ledger.append(
                        req.mode.as_str(),
                        ReasoningPhase::Final,
                        "",
                        &answer,
                        0,
                        0,
                        0.0,
                    )?;
                    self.fsm_transition(AgentState::Evaluation)?;
                    return Ok(Self::ok_outcome(answer));
                }
                ReactDirective::Action { tool, args } => {
                    self.fsm_transition(AgentState::ReasoningAction)?;
                    let action_desc = format!("{} {}", tool, args);
                    // Falha da ação NÃO aborta o ciclo: vira observação para
                    // o LLM corrigir a rota no próximo Thought.
                    let observation = match executor.execute(&tool, &args) {
                        Ok(output) => output,
                        Err(e) => format!("ERRO: {}", e),
                    };
                    ledger.append(
                        req.mode.as_str(),
                        ReasoningPhase::Action,
                        &action_desc,
                        &observation,
                        0,
                        0,
                        0.0,
                    )?;
                    self.fsm_transition(AgentState::ReasoningObservation)?;
                    ledger.append(
                        req.mode.as_str(),
                        ReasoningPhase::Observation,
                        &action_desc,
                        &observation,
                        0,
                        0,
                        0.0,
                    )?;
                    observations.push(observation);
                }
                ReactDirective::Malformed(reason) => {
                    // Resposta fora do contrato: injeta como observação e
                    // deixa o budget limitar as tentativas.
                    let observation = format!("RESPOSTA MALFORMADA: {}", reason);
                    self.fsm_transition(AgentState::ReasoningAction)?;
                    self.fsm_transition(AgentState::ReasoningObservation)?;
                    ledger.append(
                        req.mode.as_str(),
                        ReasoningPhase::Observation,
                        "harness",
                        &observation,
                        0,
                        0,
                        0.0,
                    )?;
                    observations.push(observation);
                }
            }
        }
    }

    /// PAL: o LLM gera um programa auditável; a execução é delegada ao
    /// chamador (hypervisor) — nunca executada aqui.
    fn run_program_aided(
        &self,
        req: &ReasoningRequest,
        budget: &mut ReasoningBudget,
        ledger: &mut ReasoningLedger,
    ) -> Result<ReasoningOutcome> {
        if let BudgetVerdict::Exceeded(reason) = budget.check() {
            return Ok(Self::exhausted_outcome(reason.to_string()));
        }
        budget.consume_step();

        let user = format!("{}\n\nContexto:\n{}", req.goal, req.context);
        let (content, tin, tout, cost) = self.call_llm(req, budget, &user)?;

        let program = extract_program_block(&content);
        ledger.append(
            req.mode.as_str(),
            ReasoningPhase::Program,
            &user,
            program.as_deref().unwrap_or(&content),
            tin,
            tout,
            cost,
        )?;

        match program {
            Some(code) => {
                let mut outcome = Self::ok_outcome("PROGRAM_PENDING_EXECUTION".to_string());
                outcome.program = Some(code);
                Ok(outcome)
            }
            None => bail!("modo ProgramAided: LLM não retornou bloco ```program```"),
        }
    }

    fn ok_outcome(final_answer: String) -> ReasoningOutcome {
        ReasoningOutcome {
            final_answer,
            steps_recorded: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            budget_exceeded: None,
            chain_valid: false,
            program: None,
        }
    }

    fn exhausted_outcome(reason: String) -> ReasoningOutcome {
        ReasoningOutcome {
            final_answer: String::new(),
            steps_recorded: 0,
            total_tokens: 0,
            total_cost_usd: 0.0,
            budget_exceeded: Some(reason),
            chain_valid: false,
            program: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_provider::MockProvider;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn request(mode: PromptMode, session: &str) -> ReasoningRequest {
        ReasoningRequest {
            session_id: session.into(),
            goal: "qual a capital do Brasil?".into(),
            context: String::new(),
            mode,
            model: "mock".into(),
            budget: ReasoningBudget::new(8, 100_000, 10.0, 300),
            branches: None,
        }
    }

    #[test]
    fn direct_extrai_answer_e_registra_passo() {
        let bb = temp_bb();
        let mock = MockProvider::new("blá blá\nANSWER: Brasília");
        let service = ReasoningService::new(&mock);
        let outcome = service
            .run(&bb, request(PromptMode::Direct, "s-direct"), &DenyAllExecutor)
            .unwrap();
        assert_eq!(outcome.final_answer, "Brasília");
        assert_eq!(outcome.steps_recorded, 1);
        assert!(outcome.chain_valid);
        assert!(outcome.budget_exceeded.is_none());
        assert!(outcome.total_tokens > 0);
    }

    #[test]
    fn cot_registra_thought_e_final() {
        let bb = temp_bb();
        let mock = MockProvider::new("Step 1: pensar\nStep 2: concluir\nANSWER: 42");
        let service = ReasoningService::new(&mock);
        let outcome = service
            .run(
                &bb,
                request(PromptMode::ChainOfThought, "s-cot"),
                &DenyAllExecutor,
            )
            .unwrap();
        assert_eq!(outcome.final_answer, "42");
        assert_eq!(outcome.steps_recorded, 2); // Thought + Final
        assert!(outcome.chain_valid);

        let ledger = ReasoningLedger::open(bb, "s-cot");
        let steps = ledger.steps();
        assert_eq!(steps[0].phase, ReasoningPhase::Thought);
        assert!(steps[0].output.contains("Step 1"));
        assert_eq!(steps[1].phase, ReasoningPhase::Final);
    }

    #[test]
    fn tot_seleciona_ramo_de_maior_score() {
        let bb = temp_bb();
        let mock = MockProvider::new("SCORE: 0.3\nANSWER: ramo-fraco");
        // O ramo 2 recebe score maior.
        mock.when("Ramo 2 de 3", "SCORE: 0.9\nANSWER: ramo-forte");
        let service = ReasoningService::new(&mock);
        let outcome = service
            .run(
                &bb,
                request(PromptMode::TreeOfThoughts, "s-tot"),
                &DenyAllExecutor,
            )
            .unwrap();
        assert_eq!(outcome.final_answer, "ramo-forte");
        // 3 ramos + seleção + final
        assert_eq!(outcome.steps_recorded, 5);
        assert!(outcome.chain_valid);
    }

    #[test]
    fn react_executa_acao_e_finaliza() {
        let bb = temp_bb();
        let mock = MockProvider::new(
            "THOUGHT: preciso consultar\nACTION: {\"tool\": \"lookup\", \"args\": {\"q\": \"capital\"}}",
        );
        // Quando a observação aparece no contexto, o LLM finaliza.
        mock.when("OBSERVATION 1: Brasília", "THOUGHT: achei\nFINAL: Brasília");
        let service = ReasoningService::new(&mock);

        let executor = |tool: &str, _args: &Value| -> Result<String> {
            assert_eq!(tool, "lookup");
            Ok("Brasília".to_string())
        };

        let outcome = service
            .run(&bb, request(PromptMode::ReActHarnessed, "s-react"), &executor)
            .unwrap();
        assert_eq!(outcome.final_answer, "Brasília");
        assert!(outcome.chain_valid);
        assert!(outcome.budget_exceeded.is_none());

        // Trilha esperada: Thought, Action, Observation, Thought, Final.
        let ledger = ReasoningLedger::open(bb, "s-react");
        let phases: Vec<ReasoningPhase> = ledger.steps().iter().map(|s| s.phase).collect();
        assert_eq!(
            phases,
            vec![
                ReasoningPhase::Thought,
                ReasoningPhase::Action,
                ReasoningPhase::Observation,
                ReasoningPhase::Thought,
                ReasoningPhase::Final,
            ]
        );
    }

    #[test]
    fn react_respeita_max_steps() {
        let bb = temp_bb();
        // O mock sempre propõe ação → loop só termina pelo budget.
        let mock = MockProvider::new(
            "THOUGHT: de novo\nACTION: {\"tool\": \"noop\", \"args\": {}}",
        );
        let service = ReasoningService::new(&mock);
        let executor = |_: &str, _: &Value| -> Result<String> { Ok("nada".into()) };

        let mut req = request(PromptMode::ReActHarnessed, "s-react-budget");
        req.budget = ReasoningBudget::new(3, 100_000, 10.0, 300);

        let outcome = service.run(&bb, req, &executor).unwrap();
        assert!(outcome.budget_exceeded.is_some());
        assert!(outcome.budget_exceeded.unwrap().contains("max_steps"));
        assert!(outcome.chain_valid);
    }

    #[test]
    fn react_dirige_estados_fsm_explicitos() {
        let bb = temp_bb();
        let fsm = Fsm::new(bb.clone());
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();

        let mock = MockProvider::new("THOUGHT: simples\nFINAL: pronto");
        let service = ReasoningService::new(&mock).with_fsm(&fsm);
        let outcome = service
            .run(
                &bb,
                request(PromptMode::ReActHarnessed, "s-react-fsm"),
                &DenyAllExecutor,
            )
            .unwrap();
        assert_eq!(outcome.final_answer, "pronto");
        // FINAL → FSM termina em Evaluation.
        assert_eq!(fsm.current(), AgentState::Evaluation);
    }

    #[test]
    fn react_acao_com_erro_vira_observacao() {
        let bb = temp_bb();
        let mock = MockProvider::new(
            "THOUGHT: tentar\nACTION: {\"tool\": \"quebrada\", \"args\": {}}",
        );
        mock.when("ERRO:", "THOUGHT: ferramenta falhou\nFINAL: sem-resposta");
        let service = ReasoningService::new(&mock);
        let executor = |_: &str, _: &Value| -> Result<String> { bail!("tool indisponível") };

        let outcome = service
            .run(&bb, request(PromptMode::ReActHarnessed, "s-react-err"), &executor)
            .unwrap();
        assert_eq!(outcome.final_answer, "sem-resposta");
    }

    #[test]
    fn program_aided_retorna_programa_sem_executar() {
        let bb = temp_bb();
        let mock = MockProvider::new(
            "segue o programa\n```program\nfn main() { println!(\"42\"); }\n```\nANSWER: PROGRAM_PENDING_EXECUTION",
        );
        let service = ReasoningService::new(&mock);
        let outcome = service
            .run(&bb, request(PromptMode::ProgramAided, "s-pal"), &DenyAllExecutor)
            .unwrap();
        assert_eq!(outcome.final_answer, "PROGRAM_PENDING_EXECUTION");
        assert!(outcome.program.unwrap().contains("fn main"));
        assert!(outcome.chain_valid);
    }

    #[test]
    fn budget_persistido_no_blackboard() {
        let bb = temp_bb();
        let mock = MockProvider::new("ANSWER: ok");
        let service = ReasoningService::new(&mock);
        service
            .run(&bb, request(PromptMode::Direct, "s-budget"), &DenyAllExecutor)
            .unwrap();
        let saved = bb.get_tuple("reasoning", "budget:s-budget").unwrap();
        assert_eq!(saved["steps_used"], 1);
    }

    #[test]
    fn parse_react_final() {
        let (thought, directive) = parse_react("THOUGHT: ok\nFINAL: resposta final");
        assert_eq!(thought, "ok");
        assert_eq!(directive, ReactDirective::Final("resposta final".into()));
    }

    #[test]
    fn parse_react_acao_com_texto_extra() {
        let (_, directive) =
            parse_react("THOUGHT: x\nACTION: {\"tool\": \"t\", \"args\": {\"a\": 1}} obrigado");
        match directive {
            ReactDirective::Action { tool, args } => {
                assert_eq!(tool, "t");
                assert_eq!(args["a"], 1);
            }
            other => panic!("esperava Action, veio {:?}", other),
        }
    }

    #[test]
    fn parse_react_malformado() {
        let (_, directive) = parse_react("texto qualquer sem contrato");
        assert!(matches!(directive, ReactDirective::Malformed(_)));
    }
}
