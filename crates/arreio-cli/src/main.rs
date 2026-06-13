mod batch_runner;
mod chat_transparent;
mod cmd_wiring;
mod docker;
mod environment;
mod hooks;
mod plugins;
mod symbion_pipeline;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use arreio_actors::{
    diff, plan_to_dag_tasks, ActorContext, ContextAssembler, Developer, Inspector, Planner,
};
use arreio_benchmark::BenchmarkSuite;
use arreio_dag::{Checkpoint, Dag, DagNode, NodeStatus, WorkspaceManager};
use arreio_fsm::{AgentState, Fsm, RecoveryAction, TransitionReason};
use arreio_gateway::GatewayServer;
use arreio_hypervisor::{Hypervisor, Watchdog};
use arreio_kernel::{Blackboard, DEFAULT_MODEL_STR};
use arreio_memory::{ProjectMemory, RecallPipeline, SifAssembler, TimelineRecorder};
use arreio_provider::{
    AnthropicProvider, AzureProvider, ChatRequest, DeepseekProvider, GoogleProvider,
    OllamaProvider, OpenAiCompatProvider, ProviderClient, RecoveryBlockManager, ToolDescriptor,
};
use arreio_scheduler::{JobSchedule, JobStatus, ArreioScheduler, ScheduledJob};
use arreio_security::{AuditCategory, AuditLog, PermissionModeId};
use arreio_skills::{AutoLearner, Curator, SkillMatcher, SkillStore};
use arreio_slicer::{ProgramSlicer, SliceCriterion, SliceDirection};
use arreio_telemetry::MetricsCollector;
use arreio_tools::{ToolPolicyPipeline, ToolRegistry};
use arreio_vault::{SecretScanner, SecretVault};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Loga erros de operações best-effort sem interromper o fluxo principal.
/// Usado para audit, metrics, timeline, hooks — onde a falha não deve
/// abortar a orquestração, mas deve ser visível no stderr.
macro_rules! log_err {
    ($ctx:expr, $op:expr) => {
        if let Err(e) = $op {
            eprintln!("[arreio] WARN em {}: {}", $ctx, e);
        }
    };
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "arreio", about = "O Arreio — Sistema Operacional para LLMs")]
#[command(arg_required_else_help = false)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Inicializa o workspace .arreio/ no diretório atual
    Init,
    /// Executa um pipeline completo a partir de uma spec
    Run {
        spec: PathBuf,
        #[arg(long, default_value = DEFAULT_MODEL_STR)]
        model: String,
        #[arg(long, default_value = "default")]
        permission_mode: String,
        #[arg(long)]
        serve: bool,
        /// Estratégia de recovery multi-modelo: none, primary-only, diversity
        #[arg(long, default_value = "none")]
        recovery_strategy: String,
        /// Intervalo em segundos para o daemon de autopoiese (0 = desabilitado)
        #[arg(long, default_value = "0")]
        daemon_interval_secs: u64,
        /// Habilita subagentes (Explore, Verification) no pipeline
        #[arg(long)]
        enable_subagents: bool,
        /// Habilita pipeline SYMBION completo (10 subsistemas cognitivos)
        #[arg(long)]
        symbion: bool,
        /// Credencial de agente (JWT) — zero-trust por invocação de tool (PVC-Q4.1)
        #[arg(long)]
        agent_credential: Option<String>,
        /// Modo de raciocínio do Developer: direct|cot|tot|react|pal (PVC-Q4.1)
        #[arg(long)]
        reasoning_mode: Option<String>,
        /// Força despacho priorizado por score mesmo sem scores registrados (PVC-Q4.1)
        #[arg(long)]
        prioritized: bool,
    },
    /// Resume um pipeline interrompido a partir do estado persistido
    Resume {
        #[arg(long, default_value = DEFAULT_MODEL_STR)]
        model: String,
        #[arg(long)]
        permission_mode: Option<String>,
        #[arg(long)]
        serve: bool,
        /// Estratégia de recovery multi-modelo: none, primary-only, diversity
        #[arg(long, default_value = "none")]
        recovery_strategy: String,
        /// Intervalo em segundos para o daemon de autopoiese (0 = desabilitado)
        #[arg(long, default_value = "0")]
        daemon_interval_secs: u64,
        /// Habilita subagentes (Explore, Verification) no pipeline
        #[arg(long)]
        enable_subagents: bool,
        /// Habilita pipeline SYMBION completo (10 subsistemas cognitivos)
        #[arg(long)]
        symbion: bool,
        /// Credencial de agente (JWT) — zero-trust por invocação de tool (PVC-Q4.1)
        #[arg(long)]
        agent_credential: Option<String>,
        /// Modo de raciocínio do Developer; se omitido, preserva o persistido (PVC-Q4.1)
        #[arg(long)]
        reasoning_mode: Option<String>,
        /// Força despacho priorizado por score mesmo sem scores registrados (PVC-Q4.1)
        #[arg(long)]
        prioritized: bool,
    },
    /// Inicia o gateway HTTP (dashboard web)
    Serve {
        #[arg(long, default_value = "7373")]
        port: u16,
    },
    /// Exibe o estado atual do DAG (Kanban ASCII)
    Status,
    /// Reverte para o último checkpoint git
    Rollback,
    /// Lista as habilidades (Tuple Space) aprendidas
    Skills,
    /// Agenda tarefas recorrentes (Automations)
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Gerencia agentes multi-role
    Agents {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Inicia o REPL interativo
    Repl,
    /// Executa pipeline SYMBION completo (10 camadas)
    Symbion { task: String },
    /// Diagnóstico completo do sistema
    Doctor,
    /// Executa batch de prompts com checkpointing
    Batch {
        dataset: PathBuf,
        #[arg(long, default_value = ".arreio/batch_checkpoint.json")]
        checkpoint: PathBuf,
        #[arg(long, default_value = DEFAULT_MODEL_STR)]
        model: String,
    },
    /// Gera arquivos Docker para deploy
    Docker {
        #[command(subcommand)]
        action: DockerAction,
    },
    /// Inicia bridges para ferramentas externas
    Bridge {
        #[command(subcommand)]
        action: BridgeAction,
    },
    /// Gerencia servidor MCP standalone
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Executa suite de benchmark via pipeline SYMBION
    Benchmark {
        #[arg(long, default_value = "all")]
        filter: String,
    },
    /// Inicia REPL conversacional transparente (padrão para usuários leigos)
    Chat {
        #[arg(long, default_value = DEFAULT_MODEL_STR)]
        model: String,
        #[arg(long)]
        resume: Option<String>,
        /// Mensagem inline (não interativo)
        #[arg(long)]
        message: Option<String>,
    },
    /// Gera artefatos PVC de comissionamento a partir de evidências reais (PVC-Q3.3)
    Commission {
        /// Raiz do código a varrer por stubs (todo!/unimplemented!/TODO/FIXME)
        #[arg(long, default_value = ".")]
        src: PathBuf,
        /// Diretório de saída dos artefatos .generated
        #[arg(long, default_value = ".arreio/commissioning")]
        out: PathBuf,
        /// Arquivo com a saída real de `cargo test` (evidência primária)
        #[arg(long)]
        test_output: Option<PathBuf>,
        /// Arquivo JSON com fluxos verificados ([{id, action, expected, observed, passed}])
        #[arg(long)]
        flows: Option<PathBuf>,
        /// Gera também o PROJECT_BRIEF (exige --title, --problem e --in-scope)
        #[arg(long)]
        pvc_id: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        problem: Option<String>,
        /// Item dentro do escopo do brief (repetível)
        #[arg(long = "in-scope")]
        in_scope: Vec<String>,
        #[arg(long, default_value = "operador")]
        owner: String,
        /// Pendência conhecida (repetível) — força "Aprovado com restrições"
        #[arg(long)]
        pending: Vec<String>,
        /// Restrição conhecida (repetível)
        #[arg(long)]
        restriction: Vec<String>,
        #[arg(long, default_value = "O Arreio")]
        system: String,
        #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
        version: String,
        #[arg(long, default_value = "local")]
        environment: String,
        /// Data ISO do relatório (default: data atual UTC)
        #[arg(long)]
        date: Option<String>,
    },
    /// Emite e verifica credenciais de agente zero-trust (PVC-Q3.2)
    Credential {
        #[command(subcommand)]
        action: CredentialAction,
    },
    /// Executa raciocínio auditável com budget e ledger hash-chain (PVC-Q2.1)
    Reason {
        /// Objetivo a raciocinar
        goal: String,
        /// Modo: direct|cot|tot|react|pal
        #[arg(long, default_value = "direct")]
        mode: String,
        #[arg(long, default_value = DEFAULT_MODEL_STR)]
        model: String,
        /// Arquivo com contexto adicional curado
        #[arg(long)]
        context: Option<PathBuf>,
        #[arg(long, default_value = "16")]
        budget_steps: u32,
        #[arg(long, default_value = "32000")]
        budget_tokens: u64,
        #[arg(long, default_value = "1.0")]
        budget_usd: f64,
        #[arg(long, default_value = "120")]
        timeout_sec: u64,
        /// Ramos para o modo tree_of_thoughts
        #[arg(long)]
        branches: Option<usize>,
        /// Identificador da sessão (chave do ledger no Blackboard)
        #[arg(long)]
        session_id: Option<String>,
        /// Executa o programa PAL em sandbox via Hypervisor (PVC-Q4.3; exige --program-runner)
        #[arg(long)]
        execute_program: bool,
        /// Interpretador escolhido pelo OPERADOR (ex.: python, node, "cmd /c") — nunca inferido do LLM
        #[arg(long)]
        program_runner: Option<String>,
        /// Extensão do arquivo do programa em .arreio/pal/
        #[arg(long, default_value = "prog")]
        program_ext: String,
        /// Timeout da execução do programa (kill ao exceder)
        #[arg(long, default_value = "30")]
        program_timeout_sec: u64,
    },
    /// Define e lista scores de priorização dos nós do DAG (PVC-Q3.1)
    Score {
        #[command(subcommand)]
        action: ScoreAction,
    },
}

#[derive(Subcommand)]
enum CredentialAction {
    /// Emite credencial JWT assinada (segredo: env ARREIO_JWT_SECRET, ≥32 chars)
    Issue {
        #[arg(long)]
        agent_id: String,
        #[arg(long, default_value = "developer")]
        role: String,
        /// Capability scope (repetível), ex.: tool:read_file, vault:read:openai*
        #[arg(long = "scope")]
        scopes: Vec<String>,
        #[arg(long, default_value = "24")]
        ttl_hours: u64,
    },
    /// Verifica um token e imprime as claims (nunca o segredo)
    Verify { token: String },
}

#[derive(Subcommand)]
enum ScoreAction {
    /// Define o score de um nó (tupla dag::score:{node_id} no Blackboard)
    Set {
        node_id: String,
        /// Urgência em [0,1] — peso 30%
        #[arg(long, default_value = "0.5")]
        urgency: f64,
        /// Importância em [0,1] — peso 30%
        #[arg(long, default_value = "0.5")]
        importance: f64,
        /// Risco em [0,1] — peso 10%, fail-fast (risco alto roda cedo)
        #[arg(long, default_value = "0.0")]
        risk: f64,
        /// Custo em [0,1] — peso 10%, invertido (barato roda antes)
        #[arg(long, default_value = "0.5")]
        cost: f64,
        /// Deadline absoluto em epoch segundos — pressão cresce nos últimos 7 dias
        #[arg(long)]
        deadline: Option<u64>,
    },
    /// Lista os nós com seus scores compostos
    List,
}

#[derive(Subcommand)]
enum McpAction {
    /// Inicia servidor MCP (stdio, http, sse)
    Serve {
        #[arg(default_value = "stdio")]
        transport: String,
        #[arg(long)]
        addr: Option<String>,
    },
}

#[derive(Subcommand)]
enum BridgeAction {
    /// Inicia MCP server stdio para Claude Code
    Claude,
    /// Inicia MCP server SSE para Cursor IDE
    Cursor {
        #[arg(long, default_value = "7374")]
        port: u16,
    },
    /// Inicia API server OpenAI-compatible para Hermes
    Hermes {
        #[arg(long, default_value = "7375")]
        port: u16,
    },
    /// Testa conexão com OpenClaw
    OpenClaw { gateway_url: String },
}

#[derive(Subcommand)]
enum DockerAction {
    /// Gera Dockerfile, docker-compose.yml e .dockerignore
    Init,
    /// Gera apenas Dockerfile
    Dockerfile,
    /// Gera apenas docker-compose.yml
    Compose,
}

#[derive(Subcommand)]
enum AgentAction {
    /// Adiciona um agente
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "general")]
        role: String,
        #[arg(long, default_value = "ollama")]
        provider: String,
        #[arg(long, default_value = DEFAULT_MODEL_STR)]
        model: String,
        #[arg(long, default_value = "workspacewrite")]
        permission: String,
    },
    /// Lista agentes registrados
    List,
    /// Remove um agente
    Remove { id: String },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Adiciona um job agendado
    Add {
        spec: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "60")]
        interval: u32,
    },
    /// Lista jobs agendados
    List,
    /// Remove um job agendado
    Remove { id: String },
}

// ── Caminhos do workspace ─────────────────────────────────────────────────────

fn arreio_dir() -> PathBuf {
    PathBuf::from(".arreio")
}

fn blackboard_path() -> PathBuf {
    arreio_dir().join("blackboard.json")
}

/// Timestamp Unix em segundos (helper usado em logs e persistência).
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn persist_security_permission_mode(bb: &Blackboard, mode: &str) -> Result<PermissionModeId> {
    let parsed = PermissionModeId::from_str(mode)
        .with_context(|| format!("permission-mode invalido: {}", mode))?;
    bb.put_tuple(
        "security",
        "permission_mode",
        serde_json::json!(parsed.as_str()),
    )?;
    Ok(parsed)
}

fn current_security_permission_mode(bb: &Blackboard) -> PermissionModeId {
    bb.get_tuple("security", "permission_mode")
        .and_then(|v| v.as_str().and_then(PermissionModeId::from_str))
        .unwrap_or(PermissionModeId::Default)
}

fn load_permission_rules(work_dir: &Path) -> Result<arreio_security::PermissionRules> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());

    let scoped = vec![
        read_rules_dir(
            Path::new("/etc/arreio/rules"),
            arreio_security::RuleScope::Managed,
        )?,
        read_rules_dir(
            &Path::new(&home).join(".arreio").join("rules"),
            arreio_security::RuleScope::User,
        )?,
        read_rules_dir(
            &work_dir.join(".arreio").join("rules"),
            arreio_security::RuleScope::Project,
        )?,
        read_rules_dir(
            &work_dir.join(".arreio").join("local").join("rules"),
            arreio_security::RuleScope::Local,
        )?,
    ];

    Ok(arreio_security::RuleMerger::merge(scoped))
}

fn read_rules_dir(
    path: &Path,
    scope: arreio_security::RuleScope,
) -> Result<arreio_security::PermissionRules> {
    let mut rules = arreio_security::PermissionRules::new();
    if !path.exists() {
        return Ok(rules);
    }

    for entry in
        fs::read_dir(path).with_context(|| format!("lendo regras em {}", path.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let content = fs::read_to_string(entry.path())?;
        for line in content.lines() {
            parse_permission_rule_line(line, scope, &mut rules);
        }
    }
    Ok(rules)
}

fn parse_permission_rule_line(
    line: &str,
    scope: arreio_security::RuleScope,
    rules: &mut arreio_security::PermissionRules,
) {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return;
    }

    let Some((kind, expr)) = line.split_once(':') else {
        return;
    };
    let Some(rule) = arreio_security::PermissionRule::parse(expr, scope) else {
        return;
    };

    match kind.trim().to_lowercase().as_str() {
        "allow" => rules.allow.push(rule),
        "ask" => rules.ask.push(rule),
        "deny" => rules.deny.push(rule),
        _ => {}
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Init) => cmd_init(),
        Some(Cmd::Run {
            spec,
            model,
            permission_mode,
            serve,
            recovery_strategy,
            daemon_interval_secs,
            enable_subagents,
            symbion,
            agent_credential,
            reasoning_mode,
            prioritized,
        }) => cmd_run(&spec, &model, &permission_mode, serve, &recovery_strategy, daemon_interval_secs, enable_subagents, symbion, agent_credential.as_deref(), reasoning_mode.as_deref(), prioritized),
        Some(Cmd::Resume {
            model,
            permission_mode,
            serve,
            recovery_strategy,
            daemon_interval_secs,
            enable_subagents,
            symbion,
            agent_credential,
            reasoning_mode,
            prioritized,
        }) => cmd_resume(&model, permission_mode.as_deref(), serve, &recovery_strategy, daemon_interval_secs, enable_subagents, symbion, agent_credential.as_deref(), reasoning_mode.as_deref(), prioritized),
        Some(Cmd::Serve { port }) => cmd_serve(port),
        Some(Cmd::Status) => cmd_status(),
        Some(Cmd::Rollback) => cmd_rollback(),
        Some(Cmd::Skills) => cmd_skills(),
        Some(Cmd::Schedule { action }) => match action {
            ScheduleAction::Add {
                spec,
                name,
                interval,
            } => cmd_schedule_add(&spec, &name, interval),
            ScheduleAction::List => cmd_schedule_list(),
            ScheduleAction::Remove { id } => cmd_schedule_remove(&id),
        },
        Some(Cmd::Agents { action }) => match action {
            AgentAction::Add {
                id,
                name,
                role,
                provider,
                model,
                permission,
            } => cmd_agent_add(&id, &name, &role, &provider, &model, &permission),
            AgentAction::List => cmd_agent_list(),
            AgentAction::Remove { id } => cmd_agent_remove(&id),
        },
        Some(Cmd::Repl) => cmd_repl(),
        Some(Cmd::Symbion { task }) => cmd_symbion(&task),
        Some(Cmd::Doctor) => cmd_doctor(),
        Some(Cmd::Batch {
            dataset,
            checkpoint,
            model,
        }) => cmd_batch(&dataset, &checkpoint, &model),
        Some(Cmd::Docker { action }) => cmd_docker(action),
        Some(Cmd::Bridge { action }) => cmd_bridge(action),
        Some(Cmd::Mcp { action }) => cmd_mcp(action),
        Some(Cmd::Benchmark { filter }) => cmd_benchmark(&filter),
        Some(Cmd::Commission {
            src,
            out,
            test_output,
            flows,
            pvc_id,
            title,
            problem,
            in_scope,
            owner,
            pending,
            restriction,
            system,
            version,
            environment,
            date,
        }) => cmd_wiring::cmd_commission(cmd_wiring::CommissionArgs {
            src,
            out,
            test_output,
            flows,
            pvc_id,
            title,
            problem,
            in_scope,
            owner,
            pending,
            restriction,
            system,
            version,
            environment,
            date,
        }),
        Some(Cmd::Credential { action }) => match action {
            CredentialAction::Issue {
                agent_id,
                role,
                scopes,
                ttl_hours,
            } => cmd_wiring::cmd_credential_issue(&agent_id, &role, &scopes, ttl_hours),
            CredentialAction::Verify { token } => cmd_wiring::cmd_credential_verify(&token),
        },
        Some(Cmd::Reason {
            goal,
            mode,
            model,
            context,
            budget_steps,
            budget_tokens,
            budget_usd,
            timeout_sec,
            branches,
            session_id,
            execute_program,
            program_runner,
            program_ext,
            program_timeout_sec,
        }) => cmd_wiring::cmd_reason(cmd_wiring::ReasonArgs {
            goal,
            mode,
            model,
            context,
            budget_steps,
            budget_tokens,
            budget_usd,
            timeout_sec,
            branches,
            session_id,
            execute_program,
            program_runner,
            program_ext,
            program_timeout_sec,
        }),
        Some(Cmd::Score { action }) => match action {
            ScoreAction::Set {
                node_id,
                urgency,
                importance,
                risk,
                cost,
                deadline,
            } => cmd_wiring::cmd_score_set(&node_id, urgency, importance, risk, cost, deadline),
            ScoreAction::List => cmd_wiring::cmd_score_list(),
        },
        Some(Cmd::Chat {
            model,
            resume: _,
            message,
        }) => {
            if let Some(msg) = message {
                let bb = Blackboard::open(&blackboard_path())?;
                chat_transparent::run_inline_chat(bb, &model, &msg)
            } else {
                let bb = Blackboard::open(&blackboard_path())?;
                chat_transparent::run_transparent_chat(bb, &model)
            }
        }
        None => {
            // Sem subcomando: entra no chat transparente (padrão para usuários leigos)
            fs::create_dir_all(arreio_dir())?;
            let bb = Blackboard::open(&blackboard_path())?;
            chat_transparent::run_transparent_chat(bb, &arreio_kernel::default_model())
        }
    }
}

// ── arreio init ─────────────────────────────────────────────────────────────────

fn cmd_init() -> Result<()> {
    fs::create_dir_all(arreio_dir().join("snapshots"))?;
    fs::create_dir_all(arreio_dir().join("skills"))?;
    let bb = Blackboard::open(&blackboard_path())?;
    bb.persist_now()?;
    println!("[arreio] workspace inicializado em {}", arreio_dir().display());
    Ok(())
}

// ── arreio run ──────────────────────────────────────────────────────────────────

/// Constrói o stack de provider baseado no modelo e estratégia de recovery.
///
/// - `model`: nome do modelo (ex: "gemma4", "gpt-4o", "claude-3.5-sonnet")
/// - `recovery_strategy`: "none" | "primary-only" | "diversity"
/// - `bb`: Blackboard para providers que precisam dele (Ollama)
/// Spawna thread do daemon de autopoiese (MAPE-K loop).
///
/// O daemon monitora métricas do sistema no Blackboard e publica alertas
/// quando detecta degradação. O loop principal verifica `autopoiesis:alert`
/// entre nós para reagir (ex: StrategicRetreat).
fn spawn_autopoiesis_daemon(bb: Blackboard, interval_secs: u64) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use arreio_autopoiesis::{AutopoieticSystem, HealthVariable};
        let mut system = AutopoieticSystem::new().with_blackboard(bb.clone());

        // Adiciona variável customizada para monitorar falhas de nós do DAG
        system.monitor.register(HealthVariable {
            name: "dag_failure_rate".to_string(),
            value: 0.0,
            threshold_min: 0.0,
            threshold_max: 0.3,
        });

        println!("[autopoiesis] Daemon iniciado (intervalo={}s)", interval_secs);

        loop {
            std::thread::sleep(std::time::Duration::from_secs(interval_secs));

            // Coleta métricas do Blackboard
            if let Some(v) = bb.get_tuple("metrics", "dag_failure_rate") {
                if let Some(rate) = v.as_f64() {
                    let _ = system.monitor.update("dag_failure_rate", rate);
                }
            }
            if let Some(v) = bb.get_tuple("metrics", "latency_ms") {
                if let Some(lat) = v.as_f64() {
                    let _ = system.monitor.update("latency_ms", lat);
                }
            }
            if let Some(v) = bb.get_tuple("metrics", "error_rate") {
                if let Some(rate) = v.as_f64() {
                    let _ = system.monitor.update("error_rate", rate);
                }
            }

            match system.tick() {
                Ok(result) => {
                    if !result.healthy {
                        let alert = format!(
                            "[autopoiesis] Sistema degradado: {}",
                            result.alerts.join("; ")
                        );
                        eprintln!("{}", alert);
                        let _ = bb.put_tuple(
                            "autopoiesis",
                            "alert",
                            serde_json::json!({
                                "level": "warning",
                                "message": alert,
                                "actions": result.actions_taken,
                                "timestamp": std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                            }),
                        );
                    } else {
                        // Limpa alerta anterior se sistema recuperou
                        let _ = bb.put_tuple("autopoiesis", "alert", serde_json::Value::Null);
                    }
                }
                Err(e) => {
                    eprintln!("[autopoiesis] Erro no tick: {}", e);
                }
            }
        }
    })
}

/// Constrói classificador YOLO com Stage 2 real usando provider barato.
///
/// Ordem de preferência:
/// 1. Anthropic Haiku (rápido e barato) se ANTHROPIC_API_KEY estiver definida.
/// 2. Ollama local (phi3 ou modelo configurado) como fallback local.
/// 3. Modo heurístico se nenhum provider estiver disponível.
fn build_yolo_stage2_classifier(bb: &Blackboard) -> arreio_security::YoloClassifier {
    let timeout_ms = std::env::var("ARREIO_YOLO_STAGE2_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let provider = Box::new(AnthropicProvider::new("api.anthropic.com", 443, key, true));
        return arreio_security::YoloClassifier::new()
            .with_llm_stage2(provider, "claude-3-haiku-20240307")
            .with_stage2_timeout(timeout_ms);
    }

    if std::env::var("ARREIO_YOLO_STAGE2_FORCE_OLLAMA").is_ok() {
        let provider = Box::new(OllamaProvider::new(bb.clone()));
        return arreio_security::YoloClassifier::new()
            .with_llm_stage2(provider, "phi3")
            .with_stage2_timeout(timeout_ms);
    }

    // Fallback heurístico: Stage 1 continua funcionando; Stage 2 fica desabilitado.
    arreio_security::YoloClassifier::new().with_stage2_disabled()
}

// ═══════════════════════════════════════════════════════════════════════════════
// StateRestorer via RecoveryCache (conecta arreio-recovery ao RecoveryBlockManager)
// ═══════════════════════════════════════════════════════════════════════════════

/// Adapter que usa `arreio_recovery::RecoveryCache` como `StateRestorer`.
/// Persiste checkpoints em `.arreio/recovery_cache/` entre tentativas de fallback.
struct RecoveryCacheStateRestorer {
    inner: std::sync::Mutex<arreio_recovery::RecoveryCache>,
}

impl RecoveryCacheStateRestorer {
    fn new(cache_dir: std::path::PathBuf) -> Self {
        Self {
            inner: std::sync::Mutex::new(arreio_recovery::RecoveryCache::new(cache_dir)),
        }
    }
}

impl arreio_provider::StateRestorer for RecoveryCacheStateRestorer {
    fn checkpoint(&self) -> Result<String> {
        let key = format!("checkpoint-{}", now());
        let state = serde_json::json!({"timestamp": now()}).to_string();
        let mut guard = self.inner.lock().map_err(|e| anyhow::anyhow!("mutex poison: {}", e))?;
        guard.save_state(&key, &state)?;
        Ok(key)
    }

    fn restore(&self, checkpoint_id: &str) -> Result<()> {
        let guard = self.inner.lock().map_err(|e| anyhow::anyhow!("mutex poison: {}", e))?;
        let _state = guard.restore_state(checkpoint_id)?;
        Ok(())
    }
}

fn build_provider_stack(
    model: &str,
    recovery_strategy: &str,
    bb: &Blackboard,
) -> Result<Box<dyn ProviderClient>> {
    let primary = build_single_provider(model, bb)?;

    if recovery_strategy == "none" {
        return Ok(primary);
    }

    let mut mgr = RecoveryBlockManager::new(primary);

    // Adiciona alternativas baseadas na estratégia
    if recovery_strategy == "diversity" {
        // Tenta adicionar providers diversos (cloud + local)
        let models = vec![
            ("ollama", "gemma4"),
            ("openai", "gpt-4o"),
            ("anthropic", "claude-3-5-sonnet-20241022"),
            ("google", "gemini-1.5-pro"),
            ("deepseek", "deepseek-chat"),
        ];
        for (provider_type, default_model) in models {
            if provider_type != model.split(':').next().unwrap_or(model) {
                if let Ok(alt) = build_single_provider_by_type(provider_type, default_model, bb) {
                    mgr = mgr.add_alternate(alt);
                }
            }
        }
    }

    Ok(Box::new(mgr))
}

/// Constrói um RecoveryBlockManager diretamente (usado pelo modo SYMBION).
/// Quando recovery_strategy != "none", injeta `RecoveryCacheStateRestorer`
/// para persistir checkpoints entre tentativas de fallback.
fn build_recovery_block_manager(
    model: &str,
    recovery_strategy: &str,
    bb: &Blackboard,
) -> Result<RecoveryBlockManager> {
    let primary = build_single_provider(model, bb)?;

    let mut mgr = RecoveryBlockManager::new(primary);

    if recovery_strategy != "none" {
        let cache_dir = arreio_dir().join("recovery_cache");
        let restorer = Box::new(RecoveryCacheStateRestorer::new(cache_dir));
        mgr = mgr.with_state_restorer(restorer);
    }

    if recovery_strategy == "diversity" {
        let models = vec![
            ("ollama", "gemma4"),
            ("openai", "gpt-4o"),
            ("anthropic", "claude-3-5-sonnet-20241022"),
            ("google", "gemini-1.5-pro"),
            ("deepseek", "deepseek-chat"),
        ];
        for (provider_type, default_model) in models {
            if provider_type != model.split(':').next().unwrap_or(model) {
                if let Ok(alt) = build_single_provider_by_type(provider_type, default_model, bb) {
                    mgr = mgr.add_alternate(alt);
                }
            }
        }
    }

    Ok(mgr)
}

/// Cria um provider único baseado em string de modelo.
fn build_single_provider(model: &str, bb: &Blackboard) -> Result<Box<dyn ProviderClient>> {
    let (provider_type, model_name) = model.split_once(':').unwrap_or(("ollama", model));
    build_single_provider_by_type(provider_type, model_name, bb)
}

/// Cria provider por tipo conhecido.
fn build_single_provider_by_type(
    provider_type: &str,
    model_name: &str,
    bb: &Blackboard,
) -> Result<Box<dyn ProviderClient>> {
    match provider_type {
        "mock" => Ok(Box::new(arreio_provider::MockProvider::new(model_name))),
        "ollama" => Ok(Box::new(OllamaProvider::new(bb.clone()))),
        "openai" => Ok(Box::new(OpenAiCompatProvider::new(
            "api.openai.com",
            443,
            std::env::var("OPENAI_API_KEY").ok(),
            true,
        ))),
        "anthropic" => {
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                Ok(Box::new(AnthropicProvider::new(
                    "api.anthropic.com",
                    443,
                    key,
                    true,
                )))
            } else {
                anyhow::bail!("ANTHROPIC_API_KEY não definida")
            }
        }
        "google" => {
            if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
                Ok(Box::new(GoogleProvider::new(key, model_name.to_string())))
            } else {
                anyhow::bail!("GOOGLE_API_KEY não definida")
            }
        }
        "deepseek" => {
            if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
                Ok(Box::new(DeepseekProvider::new(key)))
            } else {
                anyhow::bail!("DEEPSEEK_API_KEY não definida")
            }
        }
        "kimi" | "moonshot" => {
            if let Ok(key) = std::env::var("MOONSHOT_API_KEY") {
                Ok(Box::new(OpenAiCompatProvider::kimi(Some(key))))
            } else {
                anyhow::bail!("MOONSHOT_API_KEY não definida (necessária para kimi:<modelo>)")
            }
        }
        "minimax" => {
            if let Ok(key) = std::env::var("MINIMAX_API_KEY") {
                Ok(Box::new(OpenAiCompatProvider::minimax(Some(key))))
            } else {
                anyhow::bail!("MINIMAX_API_KEY não definida (necessária para minimax:<modelo>)")
            }
        }
        "openrouter" => {
            if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
                Ok(Box::new(OpenAiCompatProvider::openrouter(Some(key))))
            } else {
                anyhow::bail!("OPENROUTER_API_KEY não definida (necessária para openrouter:<modelo>)")
            }
        }
        "azure" => {
            let key = std::env::var("AZURE_OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("AZURE_OPENAI_API_KEY não definida"))?;
            let endpoint = std::env::var("AZURE_OPENAI_ENDPOINT").map_err(|_| {
                anyhow::anyhow!(
                    "AZURE_OPENAI_ENDPOINT não definida (ex: https://<seu-recurso>.openai.azure.com)"
                )
            })?;
            Ok(Box::new(AzureProvider::new(
                key,
                endpoint,
                model_name.to_string(),
            )))
        }
        _ => anyhow::bail!(
            "Provider desconhecido: {} (suportados: mock, ollama, openai, anthropic, google, deepseek, kimi, minimax, openrouter, azure)",
            provider_type
        ),
    }
}

fn cmd_run(
    spec_path: &Path,
    model: &str,
    permission_mode: &str,
    serve: bool,
    recovery_strategy: &str,
    daemon_interval_secs: u64,
    enable_subagents: bool,
    symbion: bool,
    agent_credential: Option<&str>,
    reasoning_mode: Option<&str>,
    prioritized: bool,
) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    persist_security_permission_mode(&bb, permission_mode)?;

    // ── PVC-Q4.1: credencial verificada ANTES de qualquer execução ──
    let credential = match agent_credential {
        Some(token) => Some(cmd_wiring::load_agent_credential(token)?),
        None => None,
    };
    // `run` sem a flag limpa o modo persistido (pipeline novo = default limpo)
    cmd_wiring::persist_reasoning_mode(&bb, reasoning_mode, true)?;

    if serve {
        spawn_background_services(bb.clone(), 8080);
    }

    // Spawn daemon de autopoiese se intervalo > 0
    if daemon_interval_secs > 0 {
        spawn_autopoiesis_daemon(bb.clone(), daemon_interval_secs);
    }

    let fsm = Fsm::new(bb.clone());

    fsm.transition(AgentState::Exploration)?;
    let spec = fs::read_to_string(spec_path)
        .with_context(|| format!("lendo spec {}", spec_path.display()))?;
    println!("[arreio] spec carregada: {} bytes", spec.len());

    fsm.transition(AgentState::Planning)?;

    // Planejador gera Plan estruturado com milestones
    let planner = Planner::new(build_single_provider(model, &bb)?, model);
    let plan = planner
        .plan(&spec)
        .context("Planejador falhou ao gerar o plano")?;
    println!(
        "[arreio] Plano gerado: '{}' com {} milestones",
        plan.goal,
        plan.milestones.len()
    );

    // Persiste o plano na memória durável do projeto
    let project_memory = ProjectMemory::open(&PathBuf::from("."))?;
    project_memory.write_prompt(&format!(
        "# Goal\n{}\n\n# Non-Goals\n{}\n\n# Constraints\n{}",
        plan.goal,
        plan.non_goals.join("\n"),
        plan.constraints.join("\n"),
    ))?;
    let plan_md = serde_json::to_string_pretty(&plan)?;
    project_memory.write_plan(&plan_md)?;

    // Deriva DAG do plano
    let tasks = plan_to_dag_tasks(&plan);
    println!("[arreio] {} tarefas derivadas do plano", tasks.len());

    // Guarda referência para persistir rationales depois
    let tasks_for_rationale: Vec<_> = tasks.iter().map(|t| t.id.clone()).collect();

    let nodes: Vec<arreio_dag::DagNode> = tasks
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            let ms = plan.milestones.get(i);
            arreio_dag::DagNode {
                id: t.id.clone(),
                title: t.title.clone(),
                depends_on: t.depends_on,
                status: NodeStatus::Waiting,
                actor_type: t.actor_type,
                file_target: t.file_target,
                instruction: t.instruction.clone(),
                payload: serde_json::json!({ "instruction": t.instruction }),
                validation_cmd: ms.and_then(|m| m.validation_cmd.clone()),
                acceptance_criteria: ms
                    .map(|m| m.acceptance_criteria.clone())
                    .unwrap_or_default(),
                decision_log: vec![],
                assigned_agent: None,
                retry_count: 0,
                contracts: t.contracts.clone(),
            }
        })
        .collect();

    let mut dag = Dag::new(nodes, bb.clone())?;

    // ── Persiste raciocínio do Arquiteto no Blackboard ──
    // Estes metadados são usados pelo harness para enriquecer o ActorContext
    // de cada Developer (Nível 3: coerência multi-passo).
    let rationale_text = format!(
        "Objetivo: {}\n\nNon-Goals: {}\n\nRestrições: {}",
        plan.goal,
        plan.non_goals.join("; "),
        plan.constraints.join("; ")
    );
    bb.put_tuple("dag", "rationale", serde_json::json!(rationale_text))?;

    // Spec original (para Developer manter coerência com o pedido inicial)
    bb.put_tuple("dag", "original_spec", serde_json::json!(spec))?;

    // Raciocínio por milestone (para dependencies_summary granular)
    for (i, ms) in plan.milestones.iter().enumerate() {
        if let Some(node_id) = tasks_for_rationale.get(i) {
            bb.put_tuple(
                "dag",
                &format!("node_rationale_{}", node_id),
                serde_json::json!({
                    "milestone": ms.title,
                    "description": ms.description,
                    "acceptance_criteria": ms.acceptance_criteria,
                }),
            )?;
        }
    }

    let dev_provider = build_provider_stack(model, recovery_strategy, &bb)?;
    let insp_provider = build_provider_stack(model, recovery_strategy, &bb)?;
    execution_loop_with_providers(
        &mut dag,
        &fsm,
        &bb,
        model,
        recovery_strategy,
        dev_provider,
        insp_provider,
        enable_subagents,
        symbion,
        credential,
        prioritized,
    )
}

// ── arreio resume ───────────────────────────────────────────────────────────────

fn cmd_resume(
    model: &str,
    permission_mode: Option<&str>,
    serve: bool,
    recovery_strategy: &str,
    daemon_interval_secs: u64,
    enable_subagents: bool,
    symbion: bool,
    agent_credential: Option<&str>,
    reasoning_mode: Option<&str>,
    prioritized: bool,
) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    if let Some(mode) = permission_mode {
        persist_security_permission_mode(&bb, mode)?;
    }

    // ── PVC-Q4.1: credencial verificada ANTES de qualquer execução ──
    let credential = match agent_credential {
        Some(token) => Some(cmd_wiring::load_agent_credential(token)?),
        None => None,
    };
    // `resume` sem a flag preserva o modo persistido (espelha permission_mode)
    cmd_wiring::persist_reasoning_mode(&bb, reasoning_mode, false)?;

    if serve {
        spawn_background_services(bb.clone(), 8080);
    }

    // Spawn daemon de autopoiese se intervalo > 0
    if daemon_interval_secs > 0 {
        spawn_autopoiesis_daemon(bb.clone(), daemon_interval_secs);
    }

    let fsm = Fsm::new(bb.clone());
    let state = fsm.current();

    println!("[arreio] Estado persistido: {}", state);

    if state == AgentState::Consolidation {
        println!("[arreio] Pipeline já concluído. Nada para resumir.");
        return Ok(());
    }

    if state == AgentState::Idle
        || state == AgentState::Exploration
        || state == AgentState::Planning
    {
        println!("[arreio] Nenhuma execução em andamento. Use 'arreio run <spec>' primeiro.");
        return Ok(());
    }

    let mut dag = Dag::load(bb.clone())?;
    if dag.nodes().is_empty() {
        println!("[arreio] DAG vazio. Não é possível resumir.");
        return Ok(());
    }

    // Reset nós Running para Waiting (foram interrompidos)
    for node in dag.nodes_mut() {
        if node.status == NodeStatus::Running {
            node.status = NodeStatus::Waiting;
        }
    }
    dag.persist()?;

    println!(
        "[arreio] Resumindo execução com {} tarefas...",
        dag.nodes().len()
    );
    let dev_provider = build_provider_stack(model, recovery_strategy, &bb)?;
    let insp_provider = build_provider_stack(model, recovery_strategy, &bb)?;
    execution_loop_with_providers(
        &mut dag,
        &fsm,
        &bb,
        model,
        recovery_strategy,
        dev_provider,
        insp_provider,
        enable_subagents,
        symbion,
        credential,
        prioritized,
    )
}

// ── Loop de execução compartilhado entre run e resume ─────────────────────────

/// Sistema prompt dinâmico para tool-use (GAP-026).
/// Usa assemble_system_prompt com SessionState derivado do contexto.
fn build_developer_system_prompt(ctx: &ActorContext, permission_mode: &str) -> String {
    let mut state = arreio_actors::SessionState::from_context(ctx, arreio_actors::ActorRole::Developer);
    state.has_tools = true;
    state.permission_mode = permission_mode.to_string();

    let mut prompt = arreio_actors::assemble_system_prompt(&state);

    // Integra hints de ambiente do EnvironmentProbe (volatile)
    let env_info = environment::EnvironmentProbe::detect();
    let hints = environment::EnvironmentProbe::system_prompt_hints(&env_info);
    if !hints.is_empty() {
        prompt.push_str("\n\n## Ambiente de Execução:\n");
        for hint in hints {
            prompt.push_str(&format!("- {}\n", hint));
        }
    }

    prompt
}

/// Resultado da execução do Developer com métricas e handoff enriquecido.
struct DeveloperResult {
    pub code: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub summary: arreio_actors::DeveloperExecutionSummary,
}

/// Executa o Developer com suporte a tool-use loop.
fn run_developer_with_tools(
    client: &dyn ProviderClient,
    model: &str,
    ctx: &ActorContext,
    registry: &ToolRegistry,
    relevant_tools: &[ToolDescriptor],
    _hypervisor: &Hypervisor,
    _work_dir: &Path,
    hook_registry: &hooks::HookRegistry,
    tool_policy: &ToolPolicyPipeline,
    permission_mode: &str,
    bb: &Blackboard,
    node_id: &str,
    reasoning_scaffold: Option<&'static str>,
) -> Result<DeveloperResult> {
    let mut summary = arreio_actors::DeveloperExecutionSummary {
        permission_mode: permission_mode.to_string(),
        ..Default::default()
    };

    if relevant_tools.is_empty() {
        // Sem tools: comportamento original via ator Developer
        let dev = Developer::new(Box::new(client.clone_box()), model);
        let code = dev.code(ctx)?;
        return Ok(DeveloperResult {
            code,
            tokens_in: 0,
            tokens_out: 0,
            summary,
        });
    }

    // Carrega tool_history persistida de tentativas anteriores (retry handoff)
    let mut tool_history = bb
        .get_tuple("dag", &format!("tool_history_{}", node_id))
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let mut total_tokens_in: u64 = 0;
    let mut total_tokens_out: u64 = 0;
    let max_iterations = 5;
    let compressor = arreio_actors::ContextCompressor::new(3, 800);
    let dev_start = std::time::Instant::now();

    for iteration in 0..max_iterations {
        // Monta user prompt com contexto + histórico de tools
        let user = format!(
            "{}

## Histórico de Ferramentas:\n{}\n\nLembre-se: quando a tarefa estiver concluída, retorne APENAS o código final.",
            build_full_user_prompt(ctx),
            if tool_history.is_empty() { "(nenhuma tool usada ainda)".to_string() } else { 
                if tool_history.len() > 3000 {
                    let messages = vec![arreio_actors::ChatMessage { role: "tool_history".into(), content: tool_history.clone() }];
                    compressor.compress(&messages)
                } else {
                    tool_history.clone()
                }
            }
        );

        // PVC-Q4.1: scaffold do --reasoning-mode anexado apenas quando a
        // tupla reasoning::mode existe e ≠ direct — default intocado.
        let mut system = build_developer_system_prompt(ctx, permission_mode);
        if let Some(scaffold) = reasoning_scaffold {
            system.push_str("\n\n## Modo de raciocínio:\n");
            system.push_str(scaffold);
        }
        let req = ChatRequest {
            messages: Vec::new(),
            model: model.to_string(),
            system,
            user,
            tools: Some(relevant_tools.to_vec()),
        };

        let resp = client.chat(req)?;
        total_tokens_in += resp.tokens_in;
        total_tokens_out += resp.tokens_out;

        // Se há tool_calls, executa com particionamento de concorrência (GAP-006)
        if let Some(ref calls) = resp.tool_calls {
            // Fase 1: policy check + hooks, coleta invocações aprovadas
            let mut approved: Vec<arreio_tools::ToolInvocation> = Vec::new();

            for (idx, call) in calls.iter().enumerate() {
                let args = serde_json::from_str(&call.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));

                let auth = tool_policy.authorize(&call.function.name, &args);
                match auth {
                    arreio_tools::ToolPolicy::Deny => {
                        let denial_msg = format!(
                            "[Tool: {}] → PERMISSION_DENIED: tool negada pela política de segurança (modo: {:?})",
                            call.function.name, "security"
                        );
                        tool_history.push_str(&format!("\n{}", denial_msg));
                        let tool_output = serde_json::json!({
                            "name": call.function.name,
                            "success": false,
                            "error": "PERMISSION_DENIED: tool negada pela política de segurança",
                            "permission_denied": true,
                        });
                        log_err!("hooks::PostToolCall(denied)", hook_registry.invoke(&hooks::HookName::PostToolCall, &tool_output));
                        continue;
                    }
                    arreio_tools::ToolPolicy::Prompt => {
                        eprintln!(
                            "[arreio] Tool {} requer aprovação (executando em modo prompt)",
                            call.function.name
                        );
                    }
                    arreio_tools::ToolPolicy::Allow => {}
                }

                let tool_input = serde_json::json!({"name": call.function.name, "arguments": args});
                let args = match hook_registry.invoke(&hooks::HookName::PreToolCall, &tool_input)? {
                    Some(modified) => modified.get("arguments").cloned().unwrap_or(args),
                    None => args,
                };

                approved.push(arreio_tools::ToolInvocation {
                    index: idx,
                    name: call.function.name.clone(),
                    arguments: args,
                });
            }

            // Fase 2: particiona reads (paralelo) vs writes (serial)
            let groups = arreio_tools::partition_invocations(approved);
            let mut all_results: Vec<arreio_tools::IndexedToolResult> = Vec::new();
            for group in &groups {
                all_results.extend(arreio_tools::execute_group(group, registry));
            }
            all_results.sort_by_key(|r| r.index);

            // Fase 3: post-processing (hooks + tool_history)
            for indexed in &all_results {
                let call = &calls[indexed.index];
                let res = &indexed.result;
                let tool_output = serde_json::json!({
                    "name": call.function.name,
                    "success": res.success,
                    "output": &res.output,
                });
                log_err!("hooks::PostToolCall", hook_registry.invoke(&hooks::HookName::PostToolCall, &tool_output));

                if let Some(ref denied) = res.permission_denied {
                    let denial_msg = format!(
                        "[Tool: {}] → PERMISSION_DENIED: {} (regra: {})",
                        call.function.name,
                        denied.reason,
                        denied.rule_matched.as_deref().unwrap_or("desconhecida")
                    );
                    tool_history.push_str(&format!("\n{}", denial_msg));
                } else {
                    let output_text = if res.success {
                        res.output.as_str()
                    } else {
                        res.error.as_deref().unwrap_or_default()
                    };
                    tool_history.push_str(&format!(
                        "\n[Tool: {}] → {}\n{}",
                        call.function.name,
                        if res.success { "OK" } else { "ERROR" },
                        output_text
                    ));
                }
            }
            continue;
        }

        // Sem tool_calls: retorna o código
        summary.tools_used = extract_tool_names_from_history(&tool_history);
        summary.tools_denied = extract_denied_tools_from_history(&tool_history);
        summary.iterations = iteration as u32 + 1;
        summary.tokens_in = total_tokens_in;
        summary.tokens_out = total_tokens_out;
        summary.duration_ms = dev_start.elapsed().as_millis() as u64;
        // Persiste tool_history para handoff entre retries
        let _ = bb.put_tuple("dag", &format!("tool_history_{}", node_id), serde_json::json!(tool_history));
        return Ok(DeveloperResult {
            code: resp.content,
            tokens_in: total_tokens_in,
            tokens_out: total_tokens_out,
            summary,
        });
    }

    summary.tools_used = extract_tool_names_from_history(&tool_history);
    summary.tools_denied = extract_denied_tools_from_history(&tool_history);
    summary.iterations = max_iterations;
    summary.tokens_in = total_tokens_in;
    summary.tokens_out = total_tokens_out;
    summary.duration_ms = dev_start.elapsed().as_millis() as u64;
    // Persiste tool_history mesmo em erro para handoff entre retries
    let _ = bb.put_tuple("dag", &format!("tool_history_{}", node_id), serde_json::json!(tool_history));
    Err(anyhow::anyhow!(
        "Tool-use loop excedeu {} iterações sem retornar código",
        max_iterations
    ))
}

/// Extrai nomes de tools usadas do histórico textual.
fn extract_tool_names_from_history(tool_history: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in tool_history.lines() {
        if let Some(start) = line.find("[Tool: ") {
            if let Some(end) = line[start..].find("] →") {
                let name = line[start + 7..start + end].trim();
                if !name.is_empty() && !names.contains(&name.to_string()) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

/// Extrai nomes de tools negadas do histórico textual.
fn extract_denied_tools_from_history(tool_history: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in tool_history.lines() {
        if line.contains("PERMISSION_DENIED") {
            if let Some(start) = line.find("[Tool: ") {
                if let Some(end) = line[start..].find("] →") {
                    let name = line[start + 7..start + end].trim();
                    if !name.is_empty() && !names.contains(&name.to_string()) {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names
}

/// Clona um ProviderClient para uso no ator Developer.
/// Nota: precisamos de um mecanismo de clone para dyn ProviderClient.
/// Como ProviderClient não tem clone, usamos um workaround via Arc quando necessário.
fn build_full_user_prompt(ctx: &ActorContext) -> String {
    let ast_section = ctx
        .ast_map
        .as_deref()
        .map(|m| {
            format!(
                "\n\n## Mapa AST atual do arquivo (assinaturas only):\n{}",
                m
            )
        })
        .unwrap_or_default();

    let memory_section = ctx
        .memory_frame
        .as_deref()
        .map(|m| format!("\n\n## Memória de Projeto Recuperada:\n{}", m))
        .unwrap_or_default();

    let skills_section = if ctx.skills_context.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", ctx.skills_context)
    };

    let agents_section = ctx
        .agents_md
        .as_deref()
        .map(|m| format!("\n\n## Instruções do Projeto:\n{}", m))
        .unwrap_or_default();

    // ── Nível 3: Coerência multi-passo ──
    let rationale_section = ctx
        .architect_rationale
        .as_deref()
        .map(|r| format!("\n\n## Por que esta tarefa existe (raciocínio do Arquiteto):\n{}", r))
        .unwrap_or_default();

    let deps_section = ctx
        .dependencies_summary
        .as_deref()
        .map(|d| format!("\n\n## O que as dependências já fizeram:\n{}", d))
        .unwrap_or_default();

    let spec_section = ctx
        .parent_spec
        .as_deref()
        .map(|s| format!("\n\n## Especificação Original (mantenha coerência):\n{}", s))
        .unwrap_or_default();

    let retry_section = ctx
        .retry_context
        .as_ref()
        .map(|rc| {
            format!(
                "\n\n## ⚠️ RETENTATIVA (tentativa {}/{}):\nErros anteriores:\n{}\nModelos já tentados: {}\nNÃO repita a mesma abordagem.",
                rc.attempt_number,
                rc.max_attempts,
                rc.previous_errors.iter().map(|e| format!("  - {}", e)).collect::<Vec<_>>().join("\n"),
                rc.models_tried.join(", ")
            )
        })
        .unwrap_or_default();

    let trajectory_section = ctx
        .trajectory_window
        .as_deref()
        .map(|t| format!("\n\n## Histórico Recente:\n{}", t))
        .unwrap_or_default();

    format!(
        "## Tarefa:\n{}{}{}{}{}{}{}{}{}{}",
        serde_json::to_string_pretty(&ctx.task_payload).unwrap_or_else(|_| "{}".to_string()),
        ast_section,
        memory_section,
        skills_section,
        agents_section,
        rationale_section,
        deps_section,
        spec_section,
        retry_section,
        trajectory_section
    )
}

// ── Skill CRUD Tool Handlers (wrapper structs para ToolRegistry) ─────────

struct SkillCreateToolHandler {
    blackboard: Blackboard,
}
impl arreio_tools::ToolHandler for SkillCreateToolHandler {
    fn handle(&self, request: arreio_tools::ToolRequest) -> anyhow::Result<arreio_tools::ToolResult> {
        match arreio_tools::skill_crud::handle_skill_create(&self.blackboard, request.arguments) {
            Ok(val) => Ok(arreio_tools::ToolResult::ok(val.to_string())),
            Err(e) => Ok(arreio_tools::ToolResult::err(e)),
        }
    }
}

struct SkillUpdateToolHandler {
    blackboard: Blackboard,
}
impl arreio_tools::ToolHandler for SkillUpdateToolHandler {
    fn handle(&self, request: arreio_tools::ToolRequest) -> anyhow::Result<arreio_tools::ToolResult> {
        match arreio_tools::skill_crud::handle_skill_update(&self.blackboard, request.arguments) {
            Ok(val) => Ok(arreio_tools::ToolResult::ok(val.to_string())),
            Err(e) => Ok(arreio_tools::ToolResult::err(e)),
        }
    }
}

struct SkillDeleteToolHandler {
    blackboard: Blackboard,
}
impl arreio_tools::ToolHandler for SkillDeleteToolHandler {
    fn handle(&self, request: arreio_tools::ToolRequest) -> anyhow::Result<arreio_tools::ToolResult> {
        match arreio_tools::skill_crud::handle_skill_delete(&self.blackboard, request.arguments) {
            Ok(val) => Ok(arreio_tools::ToolResult::ok(val.to_string())),
            Err(e) => Ok(arreio_tools::ToolResult::err(e)),
        }
    }
}

/// Usa ProgramSlicer para reduzir o contexto AST apenas às declarações relevantes
/// para a tarefa indicada pelo hint (título + instrução do nó).
fn slice_ast_context(file_path: &Path, task_hint: &str) -> Option<String> {
    let source = fs::read_to_string(file_path).ok()?;
    let lines: Vec<&str> = source.lines().collect();

    // Extrai candidatos do hint: palavras alfabéticas com >= 3 chars
    let candidates: Vec<String> = task_hint
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|w| w.trim().to_string())
        .filter(|w| w.len() >= 3 && w.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false))
        .collect();

    for candidate in candidates {
        // Procura a primeira linha que contenha o candidato
        for (idx, line) in lines.iter().enumerate() {
            if line.contains(&candidate) {
                let criterion = SliceCriterion::new(idx + 1, &candidate);
                if let Ok(result) = ProgramSlicer::slice(&source, &criterion, SliceDirection::Both)
                {
                    if !result.relevant_lines.is_empty() {
                        let mut sliced = String::new();
                        for line_no in result.relevant_lines {
                            if line_no > 0 && line_no <= lines.len() {
                                sliced.push_str(lines[line_no - 1]);
                                sliced.push('\n');
                            }
                        }
                        if !sliced.is_empty() {
                            return arreio_ast::extract_from_str(
                                &sliced,
                                &file_path.to_string_lossy(),
                            )
                            .ok()
                            .map(|m| m.to_compact_json());
                        }
                    }
                }
                break; // tenta próximo candidato se este falhar
            }
        }
    }

    // Fallback: AST completo do arquivo
    arreio_ast::extract_from_file(file_path)
        .ok()
        .map(|m| m.to_compact_json())
}

/// Aplica recovery cascade quando um nó falha.
/// Retorna true se o nó foi marcado para retry (o loop deve reprocessá-lo),
/// false se o nó foi marcado como Failed (o loop deve continuar para o próximo).
fn handle_failure_with_recovery(
    dag: &mut Dag,
    fsm: &Fsm,
    node_id: &str,
    reason: TransitionReason,
    watchdog: &mut Watchdog,
    audit: &AuditLog,
    metrics: &MetricsCollector,
    watchdog_code: i32,
    failure_reason: &str,
) -> Result<bool> {
    let action = fsm.transition_with_reason(AgentState::Correction, &reason)?;

    // Decide se tenta retry no mesmo nó ou marca como failed
    let mut should_retry = false;
    if action == RecoveryAction::Retry {
        // Procura o nó para verificar retry_count
        if let Some(node) = dag.nodes_mut().iter_mut().find(|n| n.id == node_id) {
            if node.retry_count < 3 {
                node.retry_count += 1;
                node.status = NodeStatus::Waiting;
                node.decision_log.push(format!(
                    "RECOVERY | retry {}/3 | reason={} | action={:?}",
                    node.retry_count, reason, action
                ));
                should_retry = true;
            }
        }
    }

    if !should_retry {
        dag.update_status(node_id, NodeStatus::Failed)?;
        if let Some(node) = dag.nodes_mut().iter_mut().find(|n| n.id == node_id) {
            node.decision_log.push(format!(
                "FAILED | recovery action={:?} | reason={} | {}",
                action, reason, failure_reason
            ));
        }
    }

    let _ = audit.log(
        AuditCategory::DagTransition,
        "system",
        if should_retry {
            "node_retry"
        } else {
            "node_failed"
        },
        node_id,
        serde_json::json!({
            "recovery_action": format!("{:?}", action),
            "transition_reason": format!("{:?}", reason),
            "failure_reason": failure_reason,
            "retry": should_retry,
        }),
    );
    let _ = metrics.count(
        "dag.node.failure",
        &[("node_id", node_id), ("reason", failure_reason)],
    );
    watchdog.record(watchdog_code);

    if should_retry {
        println!(
            "[arreio] Recovery: retry {}/3 para {} (action={:?})",
            dag.nodes()
                .iter()
                .find(|n| n.id == node_id)
                .map(|n| n.retry_count)
                .unwrap_or(0),
            node_id,
            action
        );
    } else {
        println!(
            "[arreio] Recovery: {:?} para {} (reason={})",
            action, node_id, reason
        );
    }

    Ok(should_retry)
}

fn execution_loop_with_providers(
    dag: &mut Dag,
    fsm: &Fsm,
    bb: &Blackboard,
    model: &str,
    recovery_strategy: &str,
    developer_client: Box<dyn arreio_provider::ProviderClient>,
    inspector_client: Box<dyn arreio_provider::ProviderClient>,
    enable_subagents: bool,
    symbion_mode: bool,
    agent_credential: Option<arreio_security::AgentCredential>,
    prioritized: bool,
) -> Result<()> {
    let work_dir = PathBuf::from(".");
    let tool_security_mode = current_security_permission_mode(bb);
    let permission_rules = load_permission_rules(&work_dir)?;
    let mut tool_policy = ToolPolicyPipeline::from_security_mode(tool_security_mode)
        .with_rules(permission_rules)
        .with_risk_context(arreio_security::SessionRiskContext {
            permission_mode: tool_security_mode.as_str().to_string(),
            workspace_root: Some(work_dir.to_string_lossy().to_string()),
            ..Default::default()
        })
        .with_classifier(build_yolo_stage2_classifier(bb));

    // ── PVC-Q4.1: zero-trust por invocação (credencial necessária, nunca
    // suficiente — regras, modos e classificador continuam valendo) ──
    if let Some(cred) = agent_credential {
        println!(
            "[arreio] credencial de agente ativa: {} (role {}, {} scopes, jti {})",
            cred.agent_id,
            cred.role,
            cred.scopes.len(),
            cred.jti
        );
        tool_policy = tool_policy.with_credential(cred);
    }
    let tool_policy = tool_policy;

    // ── PVC-Q4.1: scaffold de raciocínio persistido (None = default intocado) ──
    let reasoning_scaffold = cmd_wiring::reasoning_scaffold_from_bb(bb);
    if reasoning_scaffold.is_some() {
        println!("[arreio] modo de raciocínio ativo no Developer (tupla reasoning::mode)");
    }

    // ── Plugin Discovery + Hook Registry ─────────────────────────────────────
    let hook_registry = hooks::HookRegistry::new();
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let arreio_home = Path::new(&home).join(".arreio");
    let discovered = plugins::PluginDiscovery::discover(&arreio_home, &work_dir);
    for plugin in &discovered {
        println!(
            "[arreio] Plugin descoberto: {} @ {} ({:?})",
            plugin.manifest.name,
            plugin.path.display(),
            plugin.source
        );
        // Registra hooks reais declarados no manifesto do plugin
        plugins::PluginHookEngine::register_plugin_hooks(&hook_registry, plugin);
    }
    // Loga hooks registrados para debug
    let registered = hook_registry.registered_hooks();
    if !registered.is_empty() {
        println!("[arreio] Hooks registrados: {:?}", registered);
    }
    // Invoca OnSessionStart se houver handlers
    if hook_registry.has_hook(&hooks::HookName::OnSessionStart) {
        log_err!("hooks::OnSessionStart", hook_registry.invoke(
            &hooks::HookName::OnSessionStart,
            &serde_json::json!({"dag_nodes": dag.nodes().len()}),
        ));
    }

    // Envolve providers com hooks LLM (PreLlmCall / PostLlmCall)
    let hooked_dev = arreio_provider::HookedProvider::new(developer_client)
        .with_pre(std::sync::Arc::new({
            let reg = hook_registry.clone();
            move |req| {
                log_err!("hooks::PreLlmCall(dev)", reg.invoke(&hooks::HookName::PreLlmCall, &serde_json::json!({"model": req.model, "system": req.system, "user_preview": &req.user[..req.user.len().min(100)]})));
                Ok(())
            }
        }))
        .with_post(std::sync::Arc::new({
            let reg = hook_registry.clone();
            move |_req, resp| {
                log_err!("hooks::PostLlmCall(dev)", reg.invoke(&hooks::HookName::PostLlmCall, &serde_json::json!({"tokens_in": resp.tokens_in, "tokens_out": resp.tokens_out, "content_preview": &resp.content[..resp.content.len().min(100)]})));
                Ok(())
            }
        }));
    let hooked_insp = arreio_provider::HookedProvider::new(inspector_client)
        .with_pre(std::sync::Arc::new({
            let reg = hook_registry.clone();
            move |req| {
                log_err!("hooks::PreLlmCall(insp)", reg.invoke(&hooks::HookName::PreLlmCall, &serde_json::json!({"model": req.model, "system": req.system, "user_preview": &req.user[..req.user.len().min(100)]})));
                Ok(())
            }
        }))
        .with_post(std::sync::Arc::new({
            let reg = hook_registry.clone();
            move |_req, resp| {
                log_err!("hooks::PostLlmCall(insp)", reg.invoke(&hooks::HookName::PostLlmCall, &serde_json::json!({"tokens_in": resp.tokens_in, "tokens_out": resp.tokens_out, "content_preview": &resp.content[..resp.content.len().min(100)]})));
                Ok(())
            }
        }));

    let permission_mode = bb
        .get_tuple("config", "permission_mode")
        .and_then(|v| v.as_str().map(String::from))
        .and_then(|s| arreio_hypervisor::permissions::PermissionMode::from_str(&s))
        .unwrap_or(arreio_hypervisor::permissions::PermissionMode::WorkspaceWrite);
    let hypervisor = Hypervisor::new(default_exec_timeout())
        .with_enforcer(arreio_hypervisor::permissions::PermissionEnforcer::new(permission_mode))
        .with_sandbox();
    let mut watchdog = Watchdog::new(3, bb.clone());
    // ── SYMBION harness (opcional) ───────────────────────────────────────────
    // Quando --symbion está ativo, o pipeline SYMBION orquestra:
    // - Roteamento OODA-C por nó (FlowController)
    // - Failover multi-modelo via RecoveryBlockManager
    // - DLP (LeakPrevention) em todas as chamadas LLM
    // - Verificação de contrato pré/pós-escrita
    // - Monitoramento meta-cognitivo e autopoietic
    let mut symbion_pipeline: Option<symbion_pipeline::SymbionPipeline> = if symbion_mode {
        match build_recovery_block_manager(model, recovery_strategy, bb) {
            Ok(recovery) => {
                println!("[symbion] Pipeline SYMBION ativo com recovery strategy '{}'", recovery_strategy);
                Some(symbion_pipeline::SymbionPipeline::with_recovery_manager(bb.clone(), recovery))
            }
            Err(e) => {
                eprintln!("[symbion] AVISO: não foi possível construir provider stack ({}). Modo SYMBION desabilitado.", e);
                None
            }
        }
    } else {
        None
    };

    let dev_client_for_tools: Box<dyn arreio_provider::ProviderClient> = if let Some(ref pipeline) = symbion_pipeline {
        pipeline.recovery.clone_box()
    } else {
        hooked_dev.clone_box()
    };
    let refiner_client: Box<dyn arreio_provider::ProviderClient> = if let Some(ref pipeline) = symbion_pipeline {
        pipeline.recovery.clone_box()
    } else {
        hooked_dev.clone_box()
    };
    let inspector = Inspector::new(Box::new(hooked_insp), model);
    let audit = AuditLog::new(bb.clone(), "session-main");
    let scanner = SecretScanner::new();
    let metrics = MetricsCollector::new(bb.clone());
    let mut vault = SecretVault::open(&arreio_dir().join("vault.json"))
        .unwrap_or_else(|_| SecretVault::open(Path::new("/dev/null")).unwrap());
    let vault_known_secrets: Vec<String> = vault.list().iter().map(|e| e.value.clone()).collect();
    let project_memory = ProjectMemory::open(&work_dir)?;
    let recall = RecallPipeline::new(bb.clone());
    let sif = SifAssembler::new(800); // budget de ~800 tokens para memória
    let skill_store_for_matcher = SkillStore::new(bb.clone());
    let skill_matcher = SkillMatcher::new(skill_store_for_matcher);
    let auto_learner = AutoLearner::new(bb.clone());
    let curator = Curator::new();
    let delegate_mgr = if enable_subagents {
        Some(arreio_agents::DelegateManager::new(bb.clone()).with_max_concurrent(2))
    } else {
        None
    };
    let timeline = TimelineRecorder::new(bb.clone());
    let use_worktrees = bb
        .get_tuple("config", "use_worktrees")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut workspace_mgr = if use_worktrees {
        Some(WorkspaceManager::new(&work_dir)?)
    } else {
        None
    };

    // ── Tool Registry ────────────────────────────────────────────────────────
    let registry = ToolRegistry::new();
    let descriptors = arreio_tools::build_native_tool_descriptors();
    for desc in descriptors {
        let name = desc.function.name.clone();
        let handler: Arc<dyn arreio_tools::ToolHandler> = match name.as_str() {
            "read_file" => Arc::new(arreio_tools::ReadFileHandler),
            "write_file" => Arc::new(arreio_tools::WriteFileHandler {
                safe_root: work_dir.clone(),
            }),
            "edit_file" => Arc::new(arreio_tools::EditFileHandler {
                safe_root: work_dir.clone(),
            }),
            "apply_patch" => Arc::new(arreio_tools::ApplyPatchHandler {
                safe_root: work_dir.clone(),
            }),
            "grep_search" => Arc::new(arreio_tools::GrepSearchHandler),
            "glob_search" => Arc::new(arreio_tools::GlobSearchHandler),
            "list_dir" => Arc::new(arreio_tools::ListDirHandler),
            "exec" => {
                let timeout_secs = std::env::var("ARREIO_EXEC_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(30);
                Arc::new(arreio_tools::ExecHandler {
                    safe_root: work_dir.clone(),
                    timeout_secs,
                })
            }
            "memory_search" => Arc::new(arreio_tools::MemorySearchHandler {
                blackboard: bb.clone(),
            }),
            "memory_write" => Arc::new(arreio_tools::MemoryWriteHandler {
                blackboard: bb.clone(),
            }),
            "checkpoint_save" => Arc::new(arreio_tools::CheckpointSaveHandler),
            "checkpoint_rollback" => Arc::new(arreio_tools::CheckpointRollbackHandler),
            "web_search" => Arc::new(arreio_tools::WebSearchHandler),
            "web_fetch" => Arc::new(arreio_tools::WebFetchHandler),
            "describe_image" => Arc::new(arreio_tools::DescribeImageHandler),
            "synthesize_speech" => Arc::new(arreio_tools::SynthesizeSpeechHandler),
            "transcribe_audio" => Arc::new(arreio_tools::TranscribeAudioHandler),
            // ── Skill CRUD tools (Dev pode criar/atualizar/remover skills durante tool-use) ──
            "skill_create" => {
                let sk_bb = bb.clone();
                Arc::new(SkillCreateToolHandler { blackboard: sk_bb })
            }
            "skill_update" => {
                let sk_bb = bb.clone();
                Arc::new(SkillUpdateToolHandler { blackboard: sk_bb })
            }
            "skill_delete" => {
                let sk_bb = bb.clone();
                Arc::new(SkillDeleteToolHandler { blackboard: sk_bb })
            }
            _ => continue,
        };
        registry.register(desc, handler);
    }

    // ── Registra descritores das skill_crud tools ──
    {
        let create_desc = arreio_provider::ToolDescriptor {
            r#type: "function".to_string(),
            function: arreio_provider::ToolFunction {
                name: "skill_create".to_string(),
                description: "Cria uma nova skill procedural que será validada automaticamente".to_string(),
                parameters: arreio_tools::skill_crud::skill_create_schema(),
            },
        };
        registry.register(create_desc, Arc::new(SkillCreateToolHandler { blackboard: bb.clone() }));
        let update_desc = arreio_provider::ToolDescriptor {
            r#type: "function".to_string(),
            function: arreio_provider::ToolFunction {
                name: "skill_update".to_string(),
                description: "Atualiza os passos, descrição ou comandos de validação de uma skill existente".to_string(),
                parameters: arreio_tools::skill_crud::skill_update_schema(),
            },
        };
        registry.register(update_desc, Arc::new(SkillUpdateToolHandler { blackboard: bb.clone() }));
        let delete_desc = arreio_provider::ToolDescriptor {
            r#type: "function".to_string(),
            function: arreio_provider::ToolFunction {
                name: "skill_delete".to_string(),
                description: "Remove uma skill existente (soft-delete)".to_string(),
                parameters: arreio_tools::skill_crud::skill_delete_schema(),
            },
        };
        registry.register(delete_desc, Arc::new(SkillDeleteToolHandler { blackboard: bb.clone() }));
    }

    // ── Registra tools declaradas por plugins ──
    for plugin in &discovered {
        plugins::register_plugin_tools(&registry, plugin, &work_dir, default_exec_timeout());
    }

    log_err!("audit::execution_started", audit.log(
        AuditCategory::DagTransition,
        "system",
        "execution_started",
        "dag",
        serde_json::json!({"model": model}),
    ));

    // Context Collapse (GAP-013): inicializa colapsador para controle de crescimento do Blackboard
    let mut collapser = arreio_memory::ContextCollapser::from_env();
    // Injeta sumarizador LLM quando provider está disponível (P-007)
    if !model.starts_with("mock:") {
        let summarizer_client = dev_client_for_tools.clone_box();
        collapser = collapser.with_summarizer(Box::new(arreio_memory::LlmSummarizer::new(
            summarizer_client,
            model,
        )));
    }
    let mut node_count_since_collapse: usize = 0;

    loop {
        if bb.has_event("interrupt") {
            let event = bb.next_event("interrupt");
            log_err!("bb::interrupt_event", bb.put_tuple(
                "interrupt",
                "last_consumed",
                serde_json::json!({"event": event, "timestamp": now()}),
            ));
            fsm.interrupt()?;
            println!("[arreio] INTERRUPÇÃO: loop detectado → StrategicRetreat");
            println!("[arreio] Use 'arreio resume' para retomar ou 'arreio status' para ver o estado");
            let _ = audit.log(
                AuditCategory::FsmTransition,
                "system",
                "interrupt",
                "fsm",
                serde_json::json!({"to": "StrategicRetreat"}),
            );
            return Ok(());
        }

        // ── Verificação de alerta do daemon autopoiesis ──
        if let Some(alert_val) = bb.get_tuple("autopoiesis", "alert") {
            if let Some(level) = alert_val.get("level").and_then(|v| v.as_str()) {
                if level == "critical" {
                    eprintln!("[arreio] ALERTA CRÍTICO do daemon autopoiesis: {}",
                        alert_val.get("message").and_then(|v| v.as_str()).unwrap_or("unknown"));
                    fsm.transition(AgentState::StrategicRetreat)?;
                    return Ok(());
                } else if level == "warning" {
                    eprintln!("[arreio] AVISO do daemon autopoiesis: {}",
                        alert_val.get("message").and_then(|v| v.as_str()).unwrap_or("unknown"));
                }
            }
        }

        // PVC-Q4.1: despacho priorizado condicional — scored_ready_nodes só
        // quando há score registrado ou --prioritized; senão, ordem legada.
        let ready: Vec<String> = cmd_wiring::ordered_ready_ids(dag, prioritized, now());
        if ready.is_empty() {
            if dag.is_complete() {
                fsm.transition(AgentState::Consolidation)?;
                let _ = audit.log(
                    AuditCategory::DagTransition,
                    "system",
                    "execution_complete",
                    "dag",
                    serde_json::Value::Null,
                );
                println!("[arreio] Todas as tarefas concluídas!");
                break;
            }
            println!("[arreio] Nenhuma tarefa pronta. Verifique com 'arreio status'.");
            break;
        }

        for node_id in ready {
            // Guarda de budget: aborta o loop se o budget esgotou
            if fsm.budget().is_exhausted() {
                eprintln!("[arreio] Iteration budget exaurido. Abortando execução do DAG.");
                fsm.transition(AgentState::StrategicRetreat)
                    .unwrap_or_else(|e| eprintln!("[arreio] ERRO CRÍTICO: falha ao transitar para StrategicRetreat: {}", e));
                log_err!("audit::budget_exhausted", audit.log(
                    AuditCategory::Permission,
                    "system",
                    "budget_exhausted",
                    "dag",
                    serde_json::json!({"reason": "iteration budget exhausted"}),
                ));
                break;
            }

            let node = dag
                .nodes()
                .iter()
                .find(|n| n.id == node_id)
                .unwrap()
                .clone();
            fsm.transition_with_reason(AgentState::Execution, &TransitionReason::NextTurn)?;
            dag.update_status(&node_id, NodeStatus::Running)?;

            // Isola work_dir se worktrees estiver habilitado
            let node_work_dir = if let Some(ref mut mgr) = workspace_mgr {
                match mgr.alloc(&node_id) {
                    Ok(path) => path,
                    Err(e) => {
                        eprintln!(
                            "[arreio] Falha ao alocar worktree: {}. Usando work_dir base.",
                            e
                        );
                        work_dir.clone()
                    }
                }
            } else {
                work_dir.clone()
            };

            println!("[arreio] Executando: {} — {}", node.id, node.title);
            log_err!("timeline::node_start", timeline.record(
                "node_start",
                serde_json::json!({"node_id": &node.id, "title": &node.title}),
            ));
            log_err!("audit::node_start", audit.log(
                AuditCategory::DagTransition,
                "system",
                "node_start",
                &node.id,
                serde_json::json!({"title": &node.title}),
            ));
            log_err!("metrics::node_start", metrics.count("dag.node.start", &[("node_id", &node.id)]));

            let ast_map = node.file_target.as_deref().and_then(|p| {
                let path = Path::new(p);
                if path.extension().map(|e| e == "rs").unwrap_or(false) {
                    let hint = format!("{} {}", node.title, node.instruction);
                    slice_ast_context(path, &hint)
                } else {
                    None
                }
            });

            // Recall de memória antes de codificar
            let query = format!("{} {}", node.title, node.instruction);
            let recall_results = recall
                .recall_with_project(&query, 5, Some(&project_memory))
                .unwrap_or_default();
            let all_memories = bb
                .search_tuples("memory", "")
                .into_iter()
                .filter_map(|(_, v)| serde_json::from_value(v).ok())
                .collect::<Vec<arreio_memory::MemoryEnvelope>>();
            let project_content = project_memory.indexed_content().ok();
            let sif_frame = sif.assemble_with_project(
                &recall_results,
                &all_memories,
                project_content.as_deref(),
            );
            let memory_frame = if sif_frame.memories_included.is_empty() {
                None
            } else {
                Some(sif_frame.text)
            };

            // Skills relevantes para este nó
            let skills =
                skill_matcher.find_relevant(&format!("{} {}", node.title, node.instruction));
            let skills_context = SkillMatcher::format_context(&skills);

            // Contexto hierárquico AGENTS.md
            let assembler = ContextAssembler::new();
            let target_path = node.file_target.as_deref();
            let assembled = assembler.assemble(
                &query,
                &work_dir,
                target_path,
                &skills_context,
                memory_frame.as_deref(),
            );
            let agents_md = if assembled.to_prompt_section().is_empty() {
                None
            } else {
                Some(assembled.to_prompt_section())
            };

            // ── Nível 3: Coerência multi-passo ──
            // Raciocínio do Arquiteto (por que esta tarefa existe no DAG)
            let architect_rationale = bb
                .get_tuple("dag", "rationale")
                .and_then(|v| v.as_str().map(String::from));

            // Resumo do que as dependências já fizeram
            let dependencies_summary = if !node.depends_on.is_empty() {
                let mut parts = Vec::new();
                for dep_id in &node.depends_on {
                    if let Some(dep_value) = bb.get_tuple("dag", &format!("node_result_{}", dep_id)) {
                        let title = dep_value.get("title").and_then(|t| t.as_str()).unwrap_or(dep_id);
                        let status = dep_value.get("status").and_then(|s| s.as_str()).unwrap_or("?");
                        let files = dep_value.get("files_modified")
                            .and_then(|f| f.as_array())
                            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default();
                        parts.push(format!(
                            "- **{}** ({}): {}", dep_id, status,
                            if files.is_empty() { title.to_string() } else { format!("{} — arquivos: {}", title, files) }
                        ));
                    }
                }
                if parts.is_empty() { None } else { Some(format!("Dependências concluídas:\n{}", parts.join("\n"))) }
            } else {
                None
            };

            // Especificação original (do Blackboard, persistida na fase de planning)
            let parent_spec = bb
                .get_tuple("dag", "original_spec")
                .and_then(|v| v.as_str().map(|s| {
                    // Resume para ~300 tokens se for muito longa
                    if s.len() > 1200 { format!("{}...", &s[..1200]) } else { s.to_string() }
                }));

            // Contexto de retry — se este nó já foi executado antes
            let retry_context = if node.retry_count > 0 {
                let mut previous_errors = Vec::new();
                for i in 0..node.retry_count {
                    let error_key = format!("node_error_{}_{}", node.id, i);
                    if let Some(err_val) = bb.get_tuple("dag", &error_key) {
                        if let Some(err_str) = err_val.as_str() {
                            previous_errors.push(err_str.to_string());
                        }
                    }
                }
                // Modelos já tentados — do recovery block ou TrajectoryStore
                let models_tried = bb
                    .get_tuple("dag", &format!("node_models_{}", node.id))
                    .and_then(|v| v.as_array().cloned())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();

                Some(arreio_actors::RetryContext {
                    attempt_number: node.retry_count + 1,
                    max_attempts: 3,
                    previous_errors,
                    models_tried,
                })
            } else {
                None
            };

            // Janela de trajetória (últimos N passos do TrajectoryStore)
            let trajectory_window = {
                let ts = arreio_kernel::TrajectoryStore::new(bb.clone());
                let recent = ts.recent(5);
                if recent.is_empty() {
                    None
                } else {
                    let lines: Vec<String> = recent.iter().map(|t| {
                        format!("- {}: {} (model={}, tokens={}, {:?})",
                            t.task_id, t.specification.chars().take(80).collect::<String>(),
                            t.models_used.join(","), t.tokens_consumed, t.result)
                    }).collect();
                    Some(format!("Últimas {} execuções:\n{}", lines.len(), lines.join("\n")))
                }
            };

            // ── Subagente Explore (se habilitado e actor_type == "explore") ──
            let mut explore_context = None;
            if node.actor_type == "explore" {
                if let Some(ref mgr) = delegate_mgr {
                    println!("[arreio] Delegando nó '{}' para subagente Explore", node.id);
                    let task = arreio_agents::DelegateTask {
                        goal: node.instruction.clone(),
                        context: format!("Explore codebase for: {}", node.instruction),
                        toolsets: vec!["read_file".into(), "glob".into(), "grep".into()],
                        role: "explore".into(),
                    };
                    match mgr.delegate(&node.id, task, 0) {
                        Ok(result) => {
                            println!("[arreio] Explore result: {}", result.summary);
                            explore_context = Some(format!(
                                "## Explore Subagent Summary\n{}\n\nTools used: {}",
                                result.summary,
                                result.tool_trace.join(", ")
                            ));
                        }
                        Err(e) => {
                            eprintln!("[arreio] Explore subagent falhou: {}. Continuando com contexto base.", e);
                        }
                    }
                }
            }

            // Mescla explore_context no memory_frame se disponível
            let memory_frame = match (memory_frame, explore_context) {
                (Some(m), Some(e)) => Some(format!("{}\n\n{}", m, e)),
                (None, Some(e)) => Some(e),
                (m, None) => m,
            };

            let ctx = ActorContext {
                task_payload: node.payload.clone(),
                ast_map,
                memory_frame,
                skills_context,
                agents_md,
                architect_rationale,
                dependencies_summary,
                parent_spec,
                retry_context,
                trajectory_window,
            };

            // Seleciona tools relevantes para economia de tokens
            let relevant_tools = registry.build_tool_plan(&query, 8);
            if !relevant_tools.is_empty() {
                println!(
                    "[arreio] Tools disponíveis para este nó: {}",
                    relevant_tools
                        .iter()
                        .map(|t| t.function.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            let dev_start = std::time::Instant::now();

            // ── SYMBION: ProblemSpace decomposition + OODA-C routing por nó ────────
            let mut flow_decision = None;
            let mut symbion_task_id = None;
            if let Some(ref mut pipeline) = symbion_pipeline {
                // Decompõe instrução do nó em subobjetivos (subsistema ocioso → ativo)
                log_err!("symbion::decompose", pipeline.decompose(&node.instruction).map(|nodes| {
                    log_err!("bb::symbion_decompose", bb.put_tuple(
                        "symbion",
                        &format!("decompose_{}", node_id),
                        serde_json::json!({"subgoals": nodes.len()}),
                    ));
                }));
                let vars = arreio_ooda::EssentialVariables::new(
                    (0.0, 1.0, 0.1),
                    (0.0, 1.0, 0.8),
                    (0, 100_000, 50),
                    (0, 5000, 50),
                );
                let flow = pipeline.flow_controller.decide(&node.instruction, 0.8, Some(&vars));
                log_err!("bb::symbion_flow", bb.put_tuple(
                    "symbion",
                    &format!("flow_{}", node_id),
                    serde_json::json!({
                        "problem_space": flow.problem_space,
                        "meta_cognitive": flow.meta_cognitive,
                        "refinement": flow.refinement,
                        "recovery": flow.recovery,
                        "contract": flow.contract,
                        "supercompile": flow.supercompile,
                        "chunking": flow.chunking,
                        "autopoiesis": flow.autopoiesis,
                        "reason": flow.reason,
                    }),
                ));
                if !flow.recovery {
                    eprintln!("[symbion] Nó '{}' em emergência: {}. Abortando.", node_id, flow.reason);
                    let should_retry = handle_failure_with_recovery(
                        dag, fsm, &node_id, TransitionReason::ReactiveCompactRetry,
                        &mut watchdog, &audit, &metrics, 5, "symbion_emergency",
                    )?;
                    if should_retry { continue; }
                    continue;
                }
                flow_decision = Some(flow);
                symbion_task_id = Some(pipeline.register_task(&node_id, &node.instruction));
            }

            let dev_result = match run_developer_with_tools(
                &*dev_client_for_tools,
                model,
                &ctx,
                &registry,
                &relevant_tools,
                &hypervisor,
                &work_dir,
                &hook_registry,
                &tool_policy,
                tool_security_mode.as_str(),
                bb,
                &node_id,
                reasoning_scaffold,
            ) {
                Ok(r) => {
                    log_err!("audit::code_generated", audit.log(
                        AuditCategory::LlmCall,
                        "developer",
                        "code_generated",
                        &node.id,
                        serde_json::json!({"bytes": r.code.len(), "tokens_in": r.tokens_in, "tokens_out": r.tokens_out}),
                    ));
                    // SYMBION: registra custo real do LLM
                    if let Some(ref mut pipeline) = symbion_pipeline {
                        let cost_usd = (r.tokens_in as f64 * 0.000003) + (r.tokens_out as f64 * 0.000015);
                        pipeline.cost_tracker.record(&node_id, model, r.tokens_in as u32, r.tokens_out as u32, cost_usd);
                    }
                    r
                }
                Err(e) => {
                    eprintln!("[arreio] Desenvolvedor falhou: {}", e);
                    let should_retry = handle_failure_with_recovery(
                        dag,
                        fsm,
                        &node_id,
                        TransitionReason::ReactiveCompactRetry,
                        &mut watchdog,
                        &audit,
                        &metrics,
                        1,
                        "llm_error",
                    )?;
                    if should_retry {
                        continue;
                    }
                    continue;
                }
            };

            // Secret scanning before any write (vault-aware: skips known secrets)
            let findings: Vec<arreio_vault::SecretFinding> = scanner
                .scan(&dev_result.code)
                .into_iter()
                .filter(|f| !vault_known_secrets.iter().any(|known| f.matched_text.contains(known) || known.contains(&f.matched_text)))
                .collect();
            // Mark newly detected secrets as exposed in vault
            let vault_entries: Vec<(String, String)> = vault.list()
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect();
            let mut exposed_names = Vec::new();
            for f in &findings {
                for (name, value) in &vault_entries {
                    if f.matched_text.contains(value) || value.contains(&f.matched_text) {
                        exposed_names.push(name.clone());
                    }
                }
            }
            for name in exposed_names {
                let _ = vault.mark_exposed(&name);
            }
            if !findings.is_empty() {
                let critical = findings
                    .iter()
                    .any(|f| f.severity == arreio_vault::SecretSeverity::Critical);
                let high = findings
                    .iter()
                    .any(|f| f.severity == arreio_vault::SecretSeverity::High);
                let findings_json: Vec<_> = findings
                    .iter()
                    .map(|f| {
                        serde_json::json!({
                            "pattern": f.pattern_name,
                            "line": f.line_number,
                            "severity": format!("{:?}", f.severity),
                            "snippet": f.matched_text.chars().take(20).collect::<String>(),
                        })
                    })
                    .collect();
                log_err!("audit::secret_detected", audit.log(
                    AuditCategory::FileWrite,
                    "scanner",
                    "secret_detected",
                    &node.id,
                    serde_json::json!({"findings": findings_json}),
                ));
                let sev_label = if critical {
                    "critical"
                } else if high {
                    "high"
                } else {
                    "medium"
                };
                log_err!("metrics::secret_scan", metrics.count(
                    "secret.scan.finding",
                    &[("severity", sev_label), ("node_id", &node_id)],
                ));

                if critical || high {
                    eprintln!("[arreio] Segredos detectados no código gerado ({} critical/high). Bloqueando escrita.", findings.len());
                    let should_retry = handle_failure_with_recovery(
                        dag,
                        fsm,
                        &node_id,
                        TransitionReason::StopHookBlocking,
                        &mut watchdog,
                        &audit,
                        &metrics,
                        3,
                        "secret_detected",
                    )?;
                    if should_retry {
                        continue;
                    }
                    continue;
                }
            }

            fsm.transition(AgentState::Evaluation)?;
            let diff = node
                .file_target
                .as_deref()
                .map(|target| {
                    diff::generate_diff(target, &dev_result.code)
                        .unwrap_or_else(|_| format!("--- generated code ---\n{}", dev_result.code))
                })
                .unwrap_or_else(|| format!("--- generated code ---\n{}", dev_result.code));
            let review =
                inspector
                    .review(&diff, Some(&dev_result.summary))
                    .unwrap_or_else(|_| arreio_actors::InspectionResult {
                        approved: false,
                        issues: vec!["Inspetor indisponível".to_string()],
                    });
            log_err!("audit::inspector_review", audit.log(
                AuditCategory::LlmCall,
                "inspector",
                "review",
                &node.id,
                serde_json::json!({"approved": review.approved, "issues": review.issues}),
            ));

            if !review.approved {
                println!("[arreio] Inspetor bloqueou: {:?}", review.issues);
                let should_retry = handle_failure_with_recovery(
                    dag,
                    fsm,
                    &node_id,
                    TransitionReason::StopHookBlocking,
                    &mut watchdog,
                    &audit,
                    &metrics,
                    2,
                    "inspector_rejected",
                )?;
                if should_retry {
                    continue;
                }
                continue;
            }

            // ── Verification Agent (GAP-027): análise adversarial ──
            if !dev_result.code.is_empty() {
                let verifier = arreio_actors::VerificationAgent::new(refiner_client.clone_box(), model);
                match verifier.verify(&dev_result.code, &node.instruction) {
                    Ok(vr) => {
                        log_err!("audit::verifier", audit.log(
                            AuditCategory::LlmCall,
                            "verifier",
                            "verify",
                            &node_id,
                            serde_json::json!({"passed": vr.passed, "bugs": vr.bugs.len()}),
                        ));
                        if !vr.passed {
                            let critical = vr.bugs.iter().any(|b| matches!(b.severity, arreio_actors::Severity::Critical | arreio_actors::Severity::High));
                            if critical {
                                eprintln!("[arreio] Verificador adversarial bloqueou: {} bugs encontrados", vr.bugs.len());
                                let should_retry = handle_failure_with_recovery(
                                    dag, fsm, &node_id,
                                    TransitionReason::StopHookBlocking,
                                    &mut watchdog, &audit, &metrics, 2, "verifier_rejected",
                                )?;
                                if should_retry { continue; }
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[arreio] Verificador adversarial indisponível: {}", e);
                    }
                }
            }

            // ── SYMBION: ContractEngine pré-escrita (somente se houver código e target)
            if let Some(ref mut pipeline) = symbion_pipeline {
                if flow_decision.as_ref().map(|f| f.contract).unwrap_or(false) && !dev_result.code.is_empty() {
                    let verify = pipeline.verify_node_output(&node_id, &node.instruction, &dev_result.code);
                    log_err!("audit::symbion_contract", audit.log(
                        AuditCategory::LlmCall,
                        "symbion",
                        "contract_verify",
                        &node_id,
                        serde_json::json!({"overall": format!("{:?}", verify.overall)}),
                    ));
                    if verify.overall != arreio_contract::ContractResult::Satisfied {
                        eprintln!("[symbion] Contrato não satisfeito para {}. Acionando retry.", node_id);
                        let should_retry = handle_failure_with_recovery(
                            dag, fsm, &node_id, TransitionReason::StopHookBlocking,
                            &mut watchdog, &audit, &metrics, 2, "symbion_contract_violated",
                        )?;
                        if should_retry { continue; }
                        continue;
                    }
                }
            }

            if let Some(target) = &node.file_target {
                if let Some(parent) = Path::new(target).parent() {
                    let _ = fs::create_dir_all(parent);
                }
                fs::write(target, &dev_result.code).with_context(|| format!("escrevendo {}", target))?;
                log_err!("audit::file_written", audit.log(
                    AuditCategory::FileWrite,
                    "developer",
                    "file_written",
                    target,
                    serde_json::json!({"node_id": &node.id, "bytes": dev_result.code.len()}),
                ));
            }

            Checkpoint::save(&node_id, &node_work_dir)?;
            log_err!("audit::checkpoint_saved", audit.log(
                AuditCategory::Config,
                "system",
                "checkpoint_saved",
                &node_id,
                serde_json::Value::Null,
            ));

            // Prioridade: validation_cmd do nó (milestone) → config global
            let global_validate = bb
                .get_tuple("config", "validate_cmd")
                .and_then(|v| v.as_str().map(String::from));
            let node_validate = node.validation_cmd.clone();
            let validate_cmd = node_validate.or(global_validate);

            let (exit_code, _stdout_str, stderr_str) = if let Some(ref cmd) = validate_cmd {
                match hypervisor.run(&cmd, Some(&node_work_dir)) {
                    Ok(r) => {
                        if r.exit_code != 0 {
                            eprintln!("[arreio] validação falhou:\n{}", r.stderr);
                        }
                        arreio_security::ExecutionForensics::record(
                            bb,
                            &cmd,
                            &r.stdout,
                            &r.stderr,
                            r.exit_code,
                            &work_dir.to_string_lossy(),
                        );
                        log_err!("audit::validation", audit.log(
                            AuditCategory::Command,
                            "hypervisor",
                            "validation",
                            &node_id,
                            serde_json::json!({"cmd": cmd, "exit_code": r.exit_code}),
                        ));
                        log_err!("metrics::validation", metrics.gauge(
                            "validation.exit_code",
                            r.exit_code as f64,
                            &[("node_id", &node_id)],
                        ));
                        (r.exit_code, r.stdout, r.stderr)
                    }
                    Err(e) => {
                        eprintln!("[arreio] erro ao validar: {}", e);
                        log_err!("audit::validation_error", audit.log(
                            AuditCategory::Command,
                            "hypervisor",
                            "validation_error",
                            &node_id,
                            serde_json::json!({"error": e.to_string()}),
                        ));
                        log_err!("metrics::validation_error", metrics.gauge(
                            "validation.exit_code", -1.0, &[("node_id", &node_id)]
                        ));
                        (1, String::new(), e.to_string())
                    }
                }
            } else {
                (0, String::new(), String::new())
            };

            if exit_code == 0 {
                dag.update_status(&node_id, NodeStatus::Success)?;
                fsm.reset_recovery_tracker()?;

                // ── SYMBION: meta-cognitive + chunking + autopoiesis + task update ───
                if let Some(ref mut pipeline) = symbion_pipeline {
                    let output_summary = dev_result.code.chars().take(500).collect::<String>();
                    log_err!("symbion::reasoning", pipeline.record_node_reasoning(
                        &node_id, &node.instruction, &output_summary, 0.9
                    ));
                    if flow_decision.as_ref().map(|f| f.chunking).unwrap_or(false) {
                        pipeline.chunk_node_experience(&node_id, &node.instruction, &output_summary);
                    }
                    if flow_decision.as_ref().map(|f| f.autopoiesis).unwrap_or(false) {
                        match pipeline.tick_health() {
                            Ok(health) => {
                                if !health.healthy {
                                    eprintln!("[symbion] Autopoiesis alert: {:?}", health.alerts);
                                    log_err!("bb::symbion_alert", bb.put_tuple(
                                        "autopoiesis",
                                        "alert",
                                        serde_json::json!({
                                            "level": "warning",
                                            "message": health.alerts.join(", "),
                                            "node_id": node_id,
                                        }),
                                    ));
                                }
                            }
                            Err(e) => eprintln!("[symbion] Autopoiesis tick error: {}", e),
                        }
                    }
                    if let Some(ref task_id) = symbion_task_id {
                        log_err!("symbion::task_update", pipeline.update_task(task_id, &output_summary, true));
                    }
                }

                // ── Nível 3: persiste resultado para dependencies_summary ──
                log_err!("bb::node_result", bb.put_tuple(
                    "dag",
                    &format!("node_result_{}", node_id),
                    serde_json::json!({
                        "title": node.title,
                        "status": "Success",
                        "files_modified": node.file_target.as_ref().map(|t| vec![t]).unwrap_or_default(),
                        "validation_cmd": validate_cmd,
                        "instruction": node.instruction,
                    }),
                ));

                // ── Trajectory Store: registra execução para Refiner ──
                let ts = arreio_kernel::TrajectoryStore::new(bb.clone());
                let traj_entry = arreio_kernel::TrajectoryEntry {
                    task_id: node_id.clone(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    specification: node.instruction.clone(),
                    contract: None,
                    generated_code_snippet: Some(dev_result.code.chars().take(500).collect()),
                    code_hash: None,
                    validation_cmd: validate_cmd.clone(),
                    result: arreio_kernel::TrajectoryResult::Success {
                        test_count: 1,
                        test_passed: 1,
                    },
                    models_used: vec![model.to_string()],
                    tokens_consumed: dev_result.tokens_in + dev_result.tokens_out,
                    duration_ms: dev_start.elapsed().as_millis() as u64,
                    attempt_number: node.retry_count + 1,
                    contract_violations: vec![],
                    hitl_status: arreio_kernel::HitlStatus::NotApplicable,
                    human_decision: None,
                };
                log_err!("trajectory::success", ts.record(&traj_entry));

                watchdog.record(0);
                log_err!("timeline::node_success", timeline.record(
                    "node_success",
                    serde_json::json!({"node_id": &node_id, "title": &node.title}),
                ));
                log_err!("audit::node_success", audit.log(
                    AuditCategory::DagTransition,
                    "system",
                    "node_success",
                    &node_id,
                    serde_json::json!({"title": &node.title}),
                ));
                log_err!("metrics::node_success", metrics.count("dag.node.success", &[("node_id", &node_id)]));
                log_err!("memory::progress", project_memory
                    .append_progress(&format!("{} | {} | SUCCESS", node_id, node.title)));
                // Auto-learning com cadência adaptativa (não aprende em toda task)
                log_err!("autolearner", auto_learner.learn_from_task_if_due(
                    &node_id,
                    &node.instruction,
                    node.file_target.as_deref(),
                    if node.file_target.is_some() {
                        Some(&dev_result.code)
                    } else {
                        None
                    },
                    validate_cmd.as_deref(),
                    vec![],
                ));
                // Merge worktree de volta se estiver usando worktrees
                if let Some(ref mut mgr) = workspace_mgr {
                    log_err!("worktree::merge_back", mgr.merge_back(&node_id));
                    log_err!("worktree::release", mgr.release(&node_id));
                }
                println!("[arreio] ✓ {}", node.title);
            } else {
                Checkpoint::rollback(&node_work_dir)?;
                let should_retry = handle_failure_with_recovery(
                    dag,
                    fsm,
                    &node_id,
                    TransitionReason::ReactiveCompactRetry,
                    &mut watchdog,
                    &audit,
                    &metrics,
                    exit_code,
                    "validation_failed",
                )?;

                if should_retry {
                    // Libera worktree para que o retry use um novo
                    if let Some(ref mut mgr) = workspace_mgr {
                        log_err!("worktree::release_retry", mgr.release(&node_id));
                    }
                    println!("[arreio] ⚠ {} — revertido, retry em andamento", node.title);
                    continue;
                }

                // Não pode retry: permanece Failed
                if let Some(n) = dag.nodes_mut().iter_mut().find(|n| n.id == node_id) {
                    n.decision_log.push(format!(
                        "FAILED | exit_code={} | stderr={}",
                        exit_code,
                        stderr_str.chars().take(200).collect::<String>()
                    ));
                    // ── Persiste erro para retry_context futuro ──
                    let error_key = format!("node_error_{}_{}", node_id, n.retry_count);
                    let _ = bb.put_tuple("dag", &error_key, serde_json::json!(
                        format!("exit_code={}: {}", exit_code, stderr_str.chars().take(300).collect::<String>())
                    ));
                    // Persiste modelos já tentados
                    let models_key = format!("node_models_{}", node_id);
                    if let Some(existing) = bb.get_tuple("dag", &models_key) {
                        let mut models: Vec<String> = existing.as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        if !models.contains(&model.to_string()) {
                            models.push(model.to_string());
                            log_err!("bb::models", bb.put_tuple("dag", &models_key, serde_json::json!(models)));
                        }
                    } else {
                        log_err!("bb::models", bb.put_tuple("dag", &models_key, serde_json::json!([model])));
                    }
                    log_err!("dag::persist", dag.persist());
                }
                log_err!("timeline::node_failed", timeline.record(
                    "node_failed",
                    serde_json::json!({"node_id": &node_id, "exit_code": exit_code}),
                ));

                // ── SYMBION: registra falha no meta-cognitivo e no TaskManager ──
                if let Some(ref mut pipeline) = symbion_pipeline {
                    let error_summary = format!("exit_code={}: {}", exit_code, stderr_str.chars().take(300).collect::<String>());
                    log_err!("symbion::reasoning_fail", pipeline.record_node_reasoning(
                        &node_id, &node.instruction, &error_summary, 0.3
                    ));
                    if let Some(ref task_id) = symbion_task_id {
                        log_err!("symbion::task_update_fail", pipeline.update_task(task_id, &error_summary, false));
                    }
                    if flow_decision.as_ref().map(|f| f.autopoiesis).unwrap_or(false) {
                        if let Err(e) = pipeline.tick_health() {
                            eprintln!("[symbion] Autopoiesis tick error: {}", e);
                        }
                    }
                }

                // ── Trajectory Store: registra falha para Refiner ──
                let ts_err = arreio_kernel::TrajectoryStore::new(bb.clone());
                let traj_err_entry = arreio_kernel::TrajectoryEntry {
                    task_id: node_id.clone(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    specification: node.instruction.clone(),
                    contract: None,
                    generated_code_snippet: None,
                    code_hash: None,
                    validation_cmd: validate_cmd.clone(),
                    result: arreio_kernel::TrajectoryResult::Failure {
                        exit_code,
                        error_summary: stderr_str.chars().take(300).collect(),
                    },
                    models_used: vec![model.to_string()],
                    tokens_consumed: dev_result.tokens_in + dev_result.tokens_out,
                    duration_ms: dev_start.elapsed().as_millis() as u64,
                    attempt_number: node.retry_count + 1,
                    contract_violations: vec![],
                    hitl_status: arreio_kernel::HitlStatus::NotApplicable,
                    human_decision: None,
                };
                log_err!("trajectory::failure", ts_err.record(&traj_err_entry));

                log_err!("audit::node_failed", audit.log(AuditCategory::DagTransition, "system", "node_failed", &node_id,
                    serde_json::json!({"exit_code": exit_code, "stderr_preview": stderr_str.chars().take(200).collect::<String>()})));
                log_err!("metrics::node_failure", metrics.count(
                    "dag.node.failure",
                    &[("node_id", &node_id), ("reason", "validation_failed")],
                ));
                log_err!("memory::decision", project_memory.append_decision(&format!(
                    "node={} | exit_code={} | stderr_preview={}",
                    node_id,
                    exit_code,
                    stderr_str.chars().take(100).collect::<String>()
                )));
                // Libera worktree em caso de falha (não faz merge)
                if let Some(ref mut mgr) = workspace_mgr {
                    log_err!("worktree::release_fail", mgr.release(&node_id));
                }
                println!(
                    "[arreio] ✗ {} — revertido para checkpoint anterior",
                    node.title
                );
            }

            // Context Collapse (GAP-013): aplica periodicamente para controlar crescimento do Blackboard
            node_count_since_collapse += 1;
            if node_count_since_collapse >= 5 {
                let collapsed = collapser.collapse(&bb, "dag");
                log_err!("bb::collapse", bb.put_tuple(
                    "dag",
                    "collapsed_summary",
                    serde_json::json!({
                        "count": collapsed.len(),
                        "timestamp": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    }),
                ));
                node_count_since_collapse = 0;
            }
        }

        // ── Refiner periódico: a cada REFINER_INTERVAL nós concluídos ──
        {
            const REFINER_INTERVAL: u32 = 10;
            let done = dag.nodes().iter().filter(|n| n.status == NodeStatus::Success || n.status == NodeStatus::Failed).count() as u32;
            let last_refiner_run = bb.get_tuple("refiner", "last_run_at")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let current_run_count = bb.get_tuple("refiner", "run_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if done >= REFINER_INTERVAL && done as u64 > last_refiner_run {
                // Só roda Refiner se houver provider disponível
                // Usa o mesmo provider configurado pelo CLI (respeita --model e --recovery-strategy)
                let refiner_actor = arreio_actors::Refiner::new(bb.clone(), refiner_client.clone_box());
                match refiner_actor.analyze() {
                    Ok(report) => {
                        if !report.failures_detected.is_empty() {
                            println!(
                                "[refiner] {} contract failures detected, {} actions taken",
                                report.failures_detected.len(),
                                report.actions_taken.len(),
                            );
                        }
                        log_err!("bb::refiner_success", bb.put_tuple("refiner", "last_run_at", serde_json::json!(done)));
                        log_err!("bb::refiner_count", bb.put_tuple("refiner", "run_count", serde_json::json!(current_run_count + 1)));

                        // Handoff ativo: reage a ações críticas do Refiner
                        for action_taken in &report.actions_taken {
                            match &action_taken.action {
                                arreio_actors::RefinerAction::EscalateToHuman { reason } => {
                                    eprintln!("[refiner] ESCALADA HUMANA: {}", reason);
                                    log_err!("audit::refiner_escalate", audit.log(
                                        AuditCategory::Permission,
                                        "refiner",
                                        "escalate_to_human",
                                        "dag",
                                        serde_json::json!({"reason": reason, "contract_hash": action_taken.contract_hash}),
                                    ));
                                    fsm.transition(AgentState::StrategicRetreat)?;
                                    return Ok(());
                                }
                                arreio_actors::RefinerAction::ReDeriveContract => {
                                    println!("[refiner] Contrato re-derivado: {}", action_taken.contract_hash);
                                    if let Some(ref new_contract) = action_taken.new_contract {
                                        log_err!("bb::refiner_contract", bb.put_tuple(
                                            "contract",
                                            &action_taken.contract_hash,
                                            new_contract.clone(),
                                        ));
                                    }
                                    // Marca nós pendentes com este contrato para retry
                                    for node in dag.nodes_mut() {
                                        if node.status == NodeStatus::Failed {
                                            node.retry_count = 0; // reset para retry com novo contrato
                                            node.status = NodeStatus::Waiting;
                                            node.decision_log.push(format!(
                                                "REFINER | ReDeriveContract {} | retry reset",
                                                action_taken.contract_hash
                                            ));
                                        }
                                    }
                                    dag.persist()?;
                                }
                                arreio_actors::RefinerAction::ContinueObserving => {}
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[refiner] erro: {}", e);
                        log_err!("bb::refiner_error", bb.put_tuple("refiner", "last_run_at", serde_json::json!(done)));
                    }
                }
            }
        }

        // ── Context Collapse (GAP-013): colapsa categorias que crescem ──
        let collapsed_dag = collapser.collapse(&bb, "dag");
        if collapsed_dag.len() < bb.search_tuples("dag", "").len() {
            log_err!("bb::collapsed_dag", bb.put_tuple(
                "context",
                "collapsed_dag",
                serde_json::json!({"entries": collapsed_dag.len(), "timestamp": now()}),
            ));
        }
        let collapsed_trajectory = collapser.collapse(&bb, "trajectory");
        if collapsed_trajectory.len() < bb.search_tuples("trajectory", "").len() {
            log_err!("bb::collapsed_trajectory", bb.put_tuple(
                "context",
                "collapsed_trajectory",
                serde_json::json!({"entries": collapsed_trajectory.len(), "timestamp": now()}),
            ));
        }
    }

    // Curadoria de skills no fim da sessão
    let skill_store_for_curator = SkillStore::new(bb.clone());
    let mut all_skills = skill_store_for_curator.list();
    let report = curator.run_on_store_skills(&mut all_skills);
    // Persiste as alterações de trust_level de volta no store
    for skill in &all_skills {
        log_err!("skill_store::save", skill_store_for_curator.save(skill));
    }
    if !report.umbrellas.is_empty() || !report.archive_candidates.is_empty() {
        println!(
            "[arreio] Curator report: {} umbrellas, {} archive candidates",
            report.umbrellas.len(),
            report.archive_candidates.len()
        );
        log_err!("bb::curator_report", bb.put_tuple(
            "curator",
            "last_report",
            serde_json::json!({
                "umbrellas": report.umbrellas.len(),
                "archive_candidates": report.archive_candidates.len(),
                "clusters": report.clusters.len(),
            }),
        ));
    }

    log_err!("hooks::OnSessionEnd", hook_registry.invoke(
        &hooks::HookName::OnSessionEnd,
        &serde_json::json!({"status": "complete", "curator": report}),
    ));
    Ok(())
}

// ── arreio serve ────────────────────────────────────────────────────────────────

/// Spawns background services (scheduler, MCP, A2A, gateway) in separate threads.
/// Used when `--serve` is passed to `arreio run` or `arreio resume`.
fn default_exec_timeout() -> u64 {
    std::env::var("ARREIO_EXEC_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
}

fn spawn_background_services(bb: Blackboard, port: u16) {
    std::thread::spawn(move || {
        let sched_bb = bb.clone();
        std::thread::spawn(move || {
            let scheduler = ArreioScheduler::new(sched_bb);
            scheduler.run_loop(|job| {
                println!("[scheduler] Executando job: {} | {}", job.name, job.command);
                let cmd_path = std::path::PathBuf::from(&job.command);
                if cmd_path.exists() {
                    let model = arreio_kernel::default_model();
                    if let Err(e) = cmd_run(&cmd_path, &model, "default", false, "none", 0, false, false, None, None, false) {
                        eprintln!("[scheduler] Job {} falhou: {}", job.id, e);
                    } else {
                        println!("[scheduler] Job {} concluído", job.id);
                    }
                } else {
                    eprintln!("[scheduler] Job {}: command '{}' não é um path válido", job.id, job.command);
                }
            });
        });

        let mcp_bb = bb.clone();
        let mcp_hv = Hypervisor::new(default_exec_timeout());
        let mcp_fsm = Fsm::new(mcp_bb.clone());
        std::thread::spawn(move || {
            let mcp = arreio_mcp_server::ArreioMcpServer::new(mcp_bb, mcp_hv, mcp_fsm);
            let addr = format!("127.0.0.1:{}", port + 1);
            println!("[mcp-server] Iniciando em http://{}", addr);
            let _ = mcp.serve(arreio_mcp_server::Transport::Http { addr });
        });

        let a2a_bb = bb.clone();
        std::thread::spawn(move || {
            let mut manager = arreio_a2a::TaskManager::new();
            manager.attach_dag_callback(move |task| {
                let mut dag = arreio_dag::Dag::load(a2a_bb.clone())?;
                let node = DagNode {
                    id: task.id.clone(),
                    title: task.spec.chars().take(80).collect::<String>(),
                    depends_on: vec![],
                    status: NodeStatus::Waiting,
                    actor_type: "developer".to_string(),
                    file_target: None,
                    instruction: task.spec.clone(),
                    payload: serde_json::json!({
                        "instruction": task.spec,
                        "requested_by": task.metadata.requested_by,
                        "source": "a2a"
                    }),
                    validation_cmd: None,
                    acceptance_criteria: vec![],
                    decision_log: vec![],
                    assigned_agent: None,
                    retry_count: 0,
                    contracts: vec![],
                };
                dag.add_node(node)?;
                println!("[a2a] Tarefa {} inserida no DAG como nó", task.id);
                Ok(())
            });
            let manager = std::sync::Arc::new(std::sync::Mutex::new(manager));
            let addr = format!("127.0.0.1:{}", port + 2);
            println!("[a2a] Iniciando em http://{}", addr);
            let _ = arreio_a2a::serve_a2a(&addr, manager);
        });

        let gateway = GatewayServer::new(bb, port);
        println!("[arreio] Gateway em background: http://127.0.0.1:{}", port);
        let _ = gateway.run();
    });
}

fn cmd_serve(port: u16) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;

    // Inicia thread do scheduler (Automations)
    let sched_bb = bb.clone();
    std::thread::spawn(move || {
        let scheduler = ArreioScheduler::new(sched_bb);
        scheduler.run_loop(|job| {
            println!("[scheduler] Executando job: {} | {}", job.name, job.command);
            // Executa o job se o command for um spec path existente
            let cmd_path = std::path::PathBuf::from(&job.command);
            if cmd_path.exists() {
                let model = arreio_kernel::default_model();
                if let Err(e) = cmd_run(&cmd_path, &model, "default", false, "none", 0, false, false, None, None, false) {
                    eprintln!("[scheduler] Job {} falhou: {}", job.id, e);
                } else {
                    println!("[scheduler] Job {} concluído", job.id);
                }
            } else {
                eprintln!(
                    "[scheduler] Job {}: command '{}' não é um path válido",
                    job.id, job.command
                );
            }
        });
    });

    // Inicia thread do MCP Server (HTTP)
    let mcp_bb = bb.clone();
    let mcp_hv = Hypervisor::new(30);
    let mcp_fsm = Fsm::new(mcp_bb.clone());
    std::thread::spawn(move || {
        let mcp = arreio_mcp_server::ArreioMcpServer::new(mcp_bb, mcp_hv, mcp_fsm);
        let addr = format!("127.0.0.1:{}", port + 1);
        println!("[mcp-server] Iniciando em http://{}", addr);
        let _ = mcp.serve(arreio_mcp_server::Transport::Http { addr });
    });

    // Inicia thread do A2A Server
    let a2a_bb = bb.clone();
    std::thread::spawn(move || {
        let mut manager = arreio_a2a::TaskManager::new();
        manager.attach_dag_callback(move |task| {
            let mut dag = arreio_dag::Dag::load(a2a_bb.clone())?;
            let node = DagNode {
                id: task.id.clone(),
                title: task.spec.chars().take(80).collect::<String>(),
                depends_on: vec![],
                status: NodeStatus::Waiting,
                actor_type: "developer".to_string(),
                file_target: None,
                instruction: task.spec.clone(),
                payload: serde_json::json!({
                    "instruction": task.spec,
                    "requested_by": task.metadata.requested_by,
                    "source": "a2a"
                }),
                validation_cmd: None,
                acceptance_criteria: vec![],
                decision_log: vec![],
                assigned_agent: None,
                retry_count: 0,
                contracts: vec![],
            };
            dag.add_node(node)?;
            println!("[a2a] Tarefa {} inserida no DAG como nó", task.id);
            Ok(())
        });
        let manager = std::sync::Arc::new(std::sync::Mutex::new(manager));
        let addr = format!("127.0.0.1:{}", port + 2);
        println!("[a2a] Iniciando em http://{}", addr);
        let _ = arreio_a2a::serve_a2a(&addr, manager);
    });

    let gateway = GatewayServer::new(bb, port);
    println!("[arreio] Iniciando gateway em http://127.0.0.1:{}", port);
    println!("[arreio] MCP server em http://127.0.0.1:{}", port + 1);
    println!("[arreio] A2A server em http://127.0.0.1:{}", port + 2);
    gateway.run()
}

// ── arreio status ───────────────────────────────────────────────────────────────

fn cmd_status() -> Result<()> {
    let bb = Blackboard::open(&blackboard_path())?;
    let dag = Dag::load(bb.clone())?;
    let fsm = Fsm::new(bb.clone());
    let s = dag.summary();

    println!("Estado FSM : {}", fsm.current());
    println!();
    println!("╔══════════╦════════════╦════════════╦══════════╗");
    println!("║  TODO    ║   DOING    ║    DONE    ║  FAILED  ║");
    println!("╠══════════╬════════════╬════════════╬══════════╣");
    println!(
        "║  {:>6}  ║  {:>8}  ║  {:>8}  ║  {:>6}  ║",
        s.todo, s.doing, s.done, s.failed
    );
    println!("╚══════════╩════════════╩════════════╩══════════╝");
    println!("Total: {}", s.total);
    println!();

    for node in dag.nodes() {
        let icon = match node.status {
            NodeStatus::Success => "✓",
            NodeStatus::Failed => "✗",
            NodeStatus::Running => "▶",
            NodeStatus::Ready => "○",
            NodeStatus::Waiting => "·",
        };
        // PVC-Q4.1: exibe o score composto quando registrado (sufixo vazio
        // quando não há score — saída legada intocada).
        let score_txt = dag
            .score_of(&node.id)
            .map(|s| format!("  [score {:.3}]", s.composite(now())))
            .unwrap_or_default();
        println!("  {} [{}] {}{}", icon, node.id, node.title, score_txt);
    }
    Ok(())
}

// ── arreio rollback ─────────────────────────────────────────────────────────────

fn cmd_rollback() -> Result<()> {
    Checkpoint::rollback(&PathBuf::from("."))?;
    println!("[arreio] revertido para o checkpoint anterior");
    Ok(())
}

// ── arreio skills ───────────────────────────────────────────────────────────────

fn cmd_skills() -> Result<()> {
    let bb = Blackboard::open(&blackboard_path())?;
    let skills = bb.search_tuples("skills", "");
    if skills.is_empty() {
        println!("[arreio] Nenhuma habilidade aprendida ainda.");
    } else {
        println!("[arreio] Habilidades no Tuple Space:");
        for (key, value) in &skills {
            println!(
                "  {} → {}",
                key,
                serde_json::to_string(value).expect("falha ao serializar skill")
            );
        }
    }
    Ok(())
}

// ── arreio schedule ─────────────────────────────────────────────────────────────

fn cmd_schedule_add(spec: &Path, name: &str, interval: u32) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let scheduler = ArreioScheduler::new(bb);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let job = ScheduledJob {
        id: format!("job-{}", now),
        name: name.into(),
        description: format!("Auto: {}", spec.display()),
        schedule: JobSchedule::IntervalMinutes(interval),
        status: JobStatus::Pending,
        command: spec.to_string_lossy().into(),
        last_run: None,
        next_run: now,
        created_at: now,
        run_count: 0,
    };
    scheduler.schedule(job)?;
    println!(
        "[arreio] Job agendado: '{}' a cada {} minutos",
        name, interval
    );
    Ok(())
}

fn cmd_schedule_list() -> Result<()> {
    let bb = Blackboard::open(&blackboard_path())?;
    let scheduler = ArreioScheduler::new(bb);
    let jobs = scheduler.list();
    if jobs.is_empty() {
        println!("[arreio] Nenhum job agendado.");
        return Ok(());
    }
    println!("[arreio] Jobs agendados:");
    for job in jobs {
        println!(
            "  {} | {} | {:?} | next_run={} | status={:?}",
            job.id, job.name, job.schedule, job.next_run, job.status
        );
    }
    Ok(())
}

fn cmd_schedule_remove(id: &str) -> Result<()> {
    let bb = Blackboard::open(&blackboard_path())?;
    let scheduler = ArreioScheduler::new(bb);
    scheduler.remove(id)?;
    println!("[arreio] Job removido: {}", id);
    Ok(())
}

// ── arreio agents ───────────────────────────────────────────────────────────────

fn cmd_agent_add(
    id: &str,
    name: &str,
    role: &str,
    provider: &str,
    model: &str,
    permission: &str,
) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let reg = arreio_agents::AgentRegistry::new(bb);
    let config = arreio_agents::AgentConfig {
        agent_id: id.into(),
        name: name.into(),
        role: arreio_agents::AgentRole::from_str(role).unwrap_or(arreio_agents::AgentRole::General),
        workspace_dir: None,
        provider: provider.into(),
        model: model.into(),
        tool_allowlist: vec![],
        permission_mode: permission.into(),
        channel_bindings: vec![],
        max_spawn_depth: 3,
    };
    reg.register(&config)?;
    println!("[arreio] Agente registrado: {} (role={})", id, role);
    Ok(())
}

fn cmd_agent_list() -> Result<()> {
    let bb = Blackboard::open(&blackboard_path())?;
    let reg = arreio_agents::AgentRegistry::new(bb);
    let agents = reg.list()?;
    if agents.is_empty() {
        println!("[arreio] Nenhum agente registrado.");
        return Ok(());
    }
    println!("[arreio] Agentes registrados:");
    for a in agents {
        println!(
            "  {} | {} | role={:?} | model={} | permission={}",
            a.agent_id, a.name, a.role, a.model, a.permission_mode
        );
    }
    Ok(())
}

fn cmd_agent_remove(id: &str) -> Result<()> {
    let bb = Blackboard::open(&blackboard_path())?;
    let reg = arreio_agents::AgentRegistry::new(bb);
    reg.remove(id)?;
    println!("[arreio] Agente removido: {}", id);
    Ok(())
}

fn cmd_repl() -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let history = std::path::Path::new(&home).join(".arreio/repl_history");
    let _ = fs::create_dir_all(history.parent().unwrap());

    // Tenta conectar ao LSP (rust-analyzer) para enriquecer o REPL
    let lsp_symbols = try_lsp_symbols();
    if !lsp_symbols.is_empty() {
        println!(
            "[arreio] REPL com LSP ativo — {} símbolos disponíveis",
            lsp_symbols.len()
        );
    }

    let repl = arreio_tui::ArreioRepl::new(bb).with_history(history.to_string_lossy());
    repl.run(|input| {
        // Processa input do usuário como uma spec simples
        println!("[arreio] Processando: {}", input);
        // Se houver símbolos LSP, tenta encontrar matches para enriquecer contexto
        let lsp_hint = find_lsp_hint(input, &lsp_symbols);
        if let Some(hint) = lsp_hint {
            println!("[arreio] LSP hint: {}", hint);
        }
        Ok(format!(
            "Recebido: {} (use 'arreio run <spec>' para executar pipeline)",
            input
        ))
    })
}

/// Tenta spawnar rust-analyzer e coletar document symbols do diretório atual.
fn try_lsp_symbols() -> Vec<String> {
    let mut symbols = Vec::new();
    let Ok(mut client) = arreio_lsp::LspClient::spawn("rust-analyzer", &[]) else {
        return symbols;
    };
    let cwd = std::env::current_dir().ok();
    let root = cwd
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    if client.initialize(&root).is_err() {
        return symbols;
    }
    // Procura por arquivos .rs no diretório atual (recursivo, limitado)
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten().take(20) {
            let path = entry.path();
            if path.extension().map(|e| e == "rs").unwrap_or(false) {
                let uri = format!("file:///{}", path.to_string_lossy().replace('\\', "/"));
                if let Ok(docs) = client.document_symbol(&uri) {
                    for sym in docs {
                        symbols.push(sym.name);
                    }
                }
            }
        }
    }
    symbols
}

/// Busca símbolos LSP relevantes para o input do usuário.
fn find_lsp_hint(input: &str, symbols: &[String]) -> Option<String> {
    let input_lower = input.to_lowercase();
    symbols
        .iter()
        .find(|s| input_lower.contains(&s.to_lowercase()))
        .cloned()
}

// ── arreio doctor ───────────────────────────────────────────────────────────────

fn cmd_doctor() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║           ARREIO DOCTOR — Diagnóstico do Sistema              ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    let mut checks_passed = 0;
    let mut checks_failed = 0;

    // 1. Workspace
    let arreio = arreio_dir();
    if arreio.exists() {
        println!("✓ Workspace inicializado: {}", arreio.display());
        checks_passed += 1;
    } else {
        println!("✗ Workspace não encontrado. Execute 'arreio init' primeiro.");
        checks_failed += 1;
    }

    // 2. Blackboard
    let bb_path = blackboard_path();
    match Blackboard::open(&bb_path) {
        Ok(bb) => {
            let tuples = bb.search_tuples("", "");
            println!("✓ Blackboard acessível ({} tuplas)", tuples.len());
            checks_passed += 1;
        }
        Err(e) => {
            println!("✗ Blackboard inacessível: {}", e);
            checks_failed += 1;
        }
    }

    // 3. Git
    match std::process::Command::new("git")
        .args(["--version"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("✓ Git disponível: {}", ver);
            checks_passed += 1;
        }
        _ => {
            println!("✗ Git não encontrado no PATH");
            checks_failed += 1;
        }
    }

    // 4. Ollama
    match std::net::TcpStream::connect("127.0.0.1:11434") {
        Ok(_) => {
            println!("✓ Ollama respondendo em 127.0.0.1:11434");
            checks_passed += 1;
        }
        Err(_) => {
            println!("⚠ Ollama não disponível em 127.0.0.1:11434 (opcional)");
        }
    }

    // 5. Vault
    let vault_path = arreio_dir().join("vault.json");
    if vault_path.exists() {
        match SecretVault::open(&vault_path) {
            Ok(_) => {
                println!("✓ Vault acessível");
                checks_passed += 1;
            }
            Err(e) => {
                println!("⚠ Vault existe mas não pode ser aberto: {}", e);
            }
        }
    } else {
        println!("⚠ Vault não configurado (execute 'arreio init')");
    }

    // 6. FSM
    if arreio.exists() {
        if let Ok(bb) = Blackboard::open(&bb_path) {
            let fsm = Fsm::new(bb);
            println!("✓ FSM estado: {}", fsm.current());
            checks_passed += 1;
        }
    }

    // 7. DAG
    if arreio.exists() {
        if let Ok(bb) = Blackboard::open(&bb_path) {
            match Dag::load(bb) {
                Ok(dag) => {
                    let s = dag.summary();
                    println!(
                        "✓ DAG carregado: TODO={} DOING={} DONE={} FAILED={}",
                        s.todo, s.doing, s.done, s.failed
                    );
                    checks_passed += 1;
                }
                Err(_) => {
                    println!("⚠ DAG vazio (normal para workspace novo)");
                }
            }
        }
    }

    // 8. Skills
    if arreio.exists() {
        if let Ok(bb) = Blackboard::open(&bb_path) {
            let skills = bb.search_tuples("skills", "");
            println!("✓ Skills: {} habilidades aprendidas", skills.len());
        }
    }

    // 9. Agentes
    if arreio.exists() {
        if let Ok(bb) = Blackboard::open(&bb_path) {
            let reg = arreio_agents::AgentRegistry::new(bb);
            match reg.list() {
                Ok(agents) => println!("✓ Agentes: {} registrados", agents.len()),
                Err(_) => println!("⚠ Não foi possível listar agentes"),
            }
        }
    }

    // 10. Ambiente
    let env_info = environment::EnvironmentProbe::detect();
    println!(
        "✓ Ambiente detectado: platform={:?} docker={} wsl={} ssh={} modal={}",
        env_info.platform, env_info.is_docker, env_info.is_wsl, env_info.is_ssh, env_info.is_modal
    );
    // Sanity check: todas as variants de PlatformHint são conhecidas
    let all_platforms = environment::PlatformHint::all();
    if all_platforms.len() < 6 {
        println!("⚠ PlatformHint incompleto: {} variants (esperado 6)", all_platforms.len());
    }

    // 11. Health probe
    if arreio.exists() {
        if let Ok(bb) = Blackboard::open(&bb_path) {
            let collector = MetricsCollector::new(bb);
            let results = arreio_telemetry::HealthProbe::check_all(None, Some(&collector));
            let overall = arreio_telemetry::HealthProbe::aggregate(&results);
            match overall {
                arreio_telemetry::HealthStatus::Healthy => {
                    println!("✓ Health check: HEALTHY");
                    checks_passed += 1;
                }
                arreio_telemetry::HealthStatus::Degraded => {
                    println!("⚠ Health check: DEGRADED");
                }
                arreio_telemetry::HealthStatus::Unhealthy => {
                    println!("✗ Health check: UNHEALTHY");
                    checks_failed += 1;
                }
            }
        }
    }

    // Resumo
    println!();
    println!("────────────────────────────────────────────────────────────────");
    println!(
        "Resumo: {} passaram, {} falharam",
        checks_passed, checks_failed
    );
    if checks_failed == 0 {
        println!("Status: ✅ Sistema saudável");
    } else {
        println!("Status: ⚠️  {} problema(s) detectado(s)", checks_failed);
    }
    println!("────────────────────────────────────────────────────────────────");

    Ok(())
}

// ── arreio symbion ──────────────────────────────────────────────────────────────

fn cmd_symbion(task: &str) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    println!("[arreio] Iniciando pipeline SYMBION para: {}", task);

    let mut pipeline = symbion_pipeline::SymbionPipeline::new(bb.clone());
    let mut fsm = Fsm::new(bb);
    match pipeline.execute_task(task, &mut fsm) {
        Ok(result) => {
            println!("[arreio] SYMBION concluído em {}ms", result.execution_time_ms);
            println!("[arreio] Health: {}", result.health_status);
            println!("[arreio] Recovery attempts: {}", result.recovery_attempts);
            if let Some(code) = result.code {
                println!("[arreio] Código gerado ({} bytes):", code.len());
                println!("{}", code);
            }
            if !result.optimizations.is_empty() {
                println!("[arreio] Otimizações: {:?}", result.optimizations);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("[arreio] SYMBION falhou: {}", e);
            Err(e)
        }
    }
}

// ── arreio mcp ──────────────────────────────────────────────────────────────────

fn cmd_mcp(action: McpAction) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let hv = Hypervisor::new(default_exec_timeout());
    let fsm = Fsm::new(bb.clone());
    let mcp = arreio_mcp_server::ArreioMcpServer::new(bb, hv, fsm);

    match action {
        McpAction::Serve { transport, addr } => {
            let transport = match transport.as_str() {
                "stdio" => arreio_mcp_server::Transport::Stdio,
                "http" => arreio_mcp_server::Transport::Http {
                    addr: addr.unwrap_or_else(|| "127.0.0.1:7374".to_string()),
                },
                "sse" => arreio_mcp_server::Transport::Sse {
                    addr: addr.unwrap_or_else(|| "127.0.0.1:7374".to_string()),
                },
                other => anyhow::bail!(
                    "Transporte MCP desconhecido: {}. Use: stdio, http, sse",
                    other
                ),
            };
            // Log vai para stderr: no transporte stdio o stdout é o canal JSON-RPC do protocolo MCP.
            eprintln!("[arreio] Iniciando MCP server...");
            mcp.serve(transport)?;
        }
    }
    Ok(())
}

// ── arreio bridge ───────────────────────────────────────────────────────────────

fn cmd_bridge(action: BridgeAction) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let hv = Hypervisor::new(default_exec_timeout());
    let fsm = Fsm::new(bb.clone());

    match action {
        BridgeAction::Claude => {
            // Log vai para stderr: o stdout é o canal JSON-RPC do MCP que o Claude Code consome.
            eprintln!("[arreio] Iniciando bridge Claude (MCP stdio)...");
            let mcp = arreio_mcp_server::ArreioMcpServer::new(bb, hv, fsm);
            let bridge = arreio_bridge_claude::ClaudeMcpServer::new(mcp);
            bridge.serve_stdio()?;
        }
        BridgeAction::Cursor { port } => {
            println!(
                "[arreio] Iniciando bridge Cursor (MCP SSE) em porta {}...",
                port
            );
            let mcp = arreio_mcp_server::ArreioMcpServer::new(bb, hv, fsm);
            let bridge =
                arreio_bridge_cursor::CursorMcpServer::new(mcp, format!("127.0.0.1:{}", port));
            bridge.serve_sse()?;
        }
        BridgeAction::Hermes { port } => {
            println!(
                "[arreio] Iniciando bridge Hermes (OpenAI API) em porta {}...",
                port
            );
            let bb = Blackboard::open(&blackboard_path())?;
            let pool = arreio_provider::ProviderPool::new(arreio_provider::FailoverStrategy::Priority)
                .add_provider(Box::new(arreio_provider::OllamaProvider::new(bb.clone())))
                .add_provider(Box::new(arreio_provider::OpenAiCompatProvider::new(
                    "api.openai.com",
                    443,
                    std::env::var("OPENAI_API_KEY").ok(),
                    true,
                )))
                .add_provider(Box::new(arreio_provider::AnthropicProvider::new(
                    "api.anthropic.com",
                    443,
                    std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
                    true,
                )))
                .add_provider(Box::new(arreio_provider::GoogleProvider::new(
                    std::env::var("GOOGLE_API_KEY").unwrap_or_default(),
                    "gemini-1.5-pro".to_string(),
                )))
                .add_provider(Box::new(arreio_provider::AzureProvider::new(
                    "https://api.openai.com".to_string(),
                    std::env::var("AZURE_API_KEY").unwrap_or_default(),
                    "gpt-4o".to_string(),
                )));
            println!(
                "[arreio] Bridge Hermes: providers Ollama/OpenAI/Anthropic/Google/Azure carregados"
            );
            let server = arreio_bridge_hermes::HermesApiServer::new(pool, port);
            server.serve()?;
        }
        BridgeAction::OpenClaw { gateway_url } => {
            println!("[arreio] Testando conexão com OpenClaw em {}...", gateway_url);
            let client = arreio_bridge_openclaw::OpenClawClient::new(gateway_url);
            match client.list_cron_jobs() {
                Ok(jobs) => println!("[arreio] ✓ Conectado. Cron jobs: {:?}", jobs),
                Err(e) => println!("[arreio] ✗ Falha: {}", e),
            }
        }
    }
    Ok(())
}

// ── arreio batch ────────────────────────────────────────────────────────────────

fn cmd_batch(dataset: &Path, checkpoint: &Path, model: &str) -> Result<()> {
    use batch_runner::BatchRunner;

    println!("[arreio] Iniciando batch: {}", dataset.display());
    let mut runner = BatchRunner::new(dataset, checkpoint)?;
    let pending_samples = runner.pending_samples();
    let pending_ids: Vec<String> = pending_samples.iter().map(|s| s.id.clone()).collect();
    let pending_prompts: std::collections::HashMap<String, String> = pending_samples
        .iter()
        .map(|s| (s.id.clone(), s.prompt.clone()))
        .collect();
    let reasoning_count = pending_samples
        .iter()
        .filter(|s| BatchRunner::has_reasoning(s))
        .count();
    println!(
        "[arreio] Samples pendentes: {}/{} | com reasoning: {}",
        pending_ids.len(),
        runner.stats().total,
        reasoning_count
    );

    if pending_ids.is_empty() {
        println!("[arreio] Batch já concluído.");
        return Ok(());
    }

    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let client = build_single_provider(model, &bb)?;

    for id in pending_ids {
        let prompt = pending_prompts.get(&id).cloned().unwrap_or_default();
        println!("[arreio] Processando {}...", id);
        let req = ChatRequest {
            messages: Vec::new(),
            model: model.to_string(),
            system: "Você é um assistente de código.".to_string(),
            user: prompt,
            tools: None,
        };

        match client.chat(req) {
            Ok(resp) => {
                println!(
                    "[arreio] ✓ {} ({} tokens)",
                    id,
                    resp.tokens_in + resp.tokens_out
                );
                runner.mark_completed(&id)?;
            }
            Err(e) => {
                eprintln!("[arreio] ✗ {}: {}", id, e);
                runner.mark_failed(&id)?;
            }
        }
    }

    let stats = runner.stats();
    println!(
        "[arreio] Batch concluído: {}/{} ({:.0}%) | falhas: {} | pendentes: {}",
        stats.completed,
        stats.total,
        stats.progress * 100.0,
        stats.failed,
        stats.pending
    );
    Ok(())
}

// ── arreio docker ───────────────────────────────────────────────────────────────

fn cmd_docker(action: DockerAction) -> Result<()> {
    match action {
        DockerAction::Init => {
            docker::write_dockerfile(Path::new("Dockerfile"))?;
            docker::write_docker_compose(Path::new("docker-compose.yml"))?;
            docker::write_dockerignore(Path::new(".dockerignore"))?;
            println!(
                "[arreio] Arquivos Docker gerados: Dockerfile, docker-compose.yml, .dockerignore"
            );
        }
        DockerAction::Dockerfile => {
            docker::write_dockerfile(Path::new("Dockerfile"))?;
            println!("[arreio] Dockerfile gerado");
        }
        DockerAction::Compose => {
            docker::write_docker_compose(Path::new("docker-compose.yml"))?;
            println!("[arreio] docker-compose.yml gerado");
        }
    }
    Ok(())
}

// ── arreio benchmark ────────────────────────────────────────────────────────────

fn cmd_benchmark(filter: &str) -> Result<()> {
    fs::create_dir_all(arreio_dir())?;
    let bb = Blackboard::open(&blackboard_path())?;
    let pipeline = std::cell::RefCell::new(symbion_pipeline::SymbionPipeline::new(bb.clone()));
    let benchmark_bb = bb.clone();

    let mut suite = BenchmarkSuite::new().with_runner(move |task| {
        let start = std::time::Instant::now();
        let spec = format!("{}: {}", task.name, task.description);
        let mut p = pipeline.borrow_mut();
        let mut fsm = Fsm::new(benchmark_bb.clone());
        match p.execute_task(&spec, &mut fsm) {
            Ok(result) => {
                let quality = result
                    .code
                    .as_deref()
                    .map(arreio_benchmark::heuristic_quality_score)
                    .unwrap_or_else(|| arreio_benchmark::heuristic_quality_score(&result.output));
                arreio_benchmark::BenchmarkResult {
                    task_id: task.id.clone(),
                    success: result.recovery_attempts <= 3 && !result.output.is_empty(),
                    latency_ms: result
                        .execution_time_ms
                        .max(start.elapsed().as_millis() as u64),
                    tokens_in: 0,
                    tokens_out: 0,
                    cost_usd: 0.0,
                    quality_score: quality,
                }
            }
            Err(_) => arreio_benchmark::BenchmarkResult {
                task_id: task.id.clone(),
                success: false,
                latency_ms: start.elapsed().as_millis() as u64,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                quality_score: 0.0,
            },
        }
    });

    if filter != "all" {
        suite
            .tasks
            .retain(|t| t.id.contains(filter) || t.name.contains(filter));
    }

    println!(
        "[arreio] Executando benchmark SYMBION com {} tarefas...",
        suite.tasks.len()
    );
    let results = suite.run();

    println!("\n========================================");
    println!("    Relatório de Benchmark SYMBION");
    println!("========================================");

    let mut total_latency = 0u64;
    let mut success_count = 0usize;

    for res in &results {
        let icon = if res.success { "✅" } else { "❌" };
        println!("\n[Tarefa {}] {}", res.task_id, icon);
        println!(
            "  latência: {:>6} ms | qualidade: {:.2}",
            res.latency_ms, res.quality_score
        );
        total_latency += res.latency_ms;
        if res.success {
            success_count += 1;
        }
    }

    println!("\n----------------------------------------");
    println!(
        "Total: {} tarefas | Sucesso: {} | Falha: {}",
        results.len(),
        success_count,
        results.len() - success_count
    );
    println!("Latência total: {} ms", total_latency);
    println!("========================================");

    Ok(())
}

// ── arreio chat (transparente) ──────────────────────────────────────────────────

// ── Testes E2E ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use arreio_provider::MockProvider;
    use tempfile::TempDir;

    fn init_git(dir: &std::path::Path) {
        let _ = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output();
        let _ = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@test",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir)
            .output();
    }

    #[test]
    fn parse_permission_rule_line_popula_listas() {
        let mut rules = arreio_security::PermissionRules::new();
        parse_permission_rule_line(
            "allow: read_file",
            arreio_security::RuleScope::Project,
            &mut rules,
        );
        parse_permission_rule_line(
            "ask: write_file(src/)",
            arreio_security::RuleScope::Project,
            &mut rules,
        );
        parse_permission_rule_line("deny: exec", arreio_security::RuleScope::Project, &mut rules);

        assert_eq!(rules.allow.len(), 1);
        assert_eq!(rules.ask[0].pattern.as_deref(), Some("src/"));
        assert_eq!(rules.deny[0].tool, "exec");
    }

    #[test]
    fn persist_security_permission_mode_rejeita_invalido() {
        let dir = TempDir::new().unwrap();
        let bb = Blackboard::open(&dir.path().join("blackboard.json")).unwrap();

        assert!(persist_security_permission_mode(&bb, "auto-classifier").is_ok());
        assert_eq!(
            current_security_permission_mode(&bb),
            PermissionModeId::AutoWithClassifier
        );
        assert!(persist_security_permission_mode(&bb, "nope").is_err());
    }

    #[test]
    fn pipeline_completo_com_mock_provider() {
        let dir = TempDir::new().unwrap();
        let work = dir.path();
        std::env::set_current_dir(work).unwrap();
        init_git(work);

        // Cria blackboard e config
        fs::create_dir_all(work.join(".arreio")).unwrap();
        let bb = Blackboard::open(&work.join(".arreio/blackboard.json")).unwrap();
        bb.put_tuple("config", "validate_cmd", serde_json::json!("echo ok"))
            .unwrap();

        let fsm = Fsm::new(bb.clone());
        fsm.transition(AgentState::Exploration).unwrap();
        fsm.transition(AgentState::Planning).unwrap();

        // Cria DAG com 1 nó simples
        let nodes = vec![arreio_dag::DagNode {
            id: "t1".into(),
            title: "hello".into(),
            depends_on: vec![],
            status: NodeStatus::Waiting,
            actor_type: "developer".into(),
            file_target: Some("hello.rs".into()),
            instruction: "Criar hello.rs com fn main()".into(),
            payload: serde_json::json!({"instruction": "Criar hello.rs com fn main()"}),
            validation_cmd: None,
            acceptance_criteria: vec!["compila".into()],
            decision_log: vec![],
            assigned_agent: None,
            retry_count: 0,
            contracts: vec![],
        }];
        let mut dag = Dag::new(nodes, bb.clone()).unwrap();

        // Mocks
        let dev_mock = MockProvider::new("fn main() { println!(\"hello\"); }");
        let insp_mock = MockProvider::new(r#"{"approved": true, "issues": []}"#);

        // Executa pipeline
        execution_loop_with_providers(
            &mut dag,
            &fsm,
            &bb,
            "mock",
            "none",
            Box::new(dev_mock),
            Box::new(insp_mock),
            false,
            false,
            None,
            false,
        )
        .unwrap();

        // Validações
        assert!(dag.is_complete(), "DAG deveria estar completo");
        let node = dag.nodes().iter().find(|n| n.id == "t1").unwrap();
        assert_eq!(node.status, NodeStatus::Success);
        assert!(
            work.join("hello.rs").exists(),
            "arquivo deveria ter sido criado"
        );

        // Verifica audit trail
        let audits = bb.search_tuples("audit", "");
        assert!(!audits.is_empty(), "deveria haver audit entries");

        // Verifica progresso
        let pm = ProjectMemory::open(work).unwrap();
        let progress = pm.read_progress().unwrap();
        assert!(
            progress.contains("SUCCESS"),
            "Progress.md deveria conter SUCCESS"
        );
    }
}
