//! Testes E2E do Wiring CLI das Fases 2–3 (PVC-Q4.1, dívida D-008).
//!
//! Exercitam o binário `arreio` real em workspaces temporários:
//! - `arreio commission` (R-F-058)
//! - `arreio credential issue|verify` + `--agent-credential` (R-F-059/060)
//! - `arreio reason` + `--reasoning-mode` (R-F-061/062)
//! - `arreio score set|list` + coluna no status (R-F-063)

use arreio_e2e_tests::{hello_world_spec, ArreioWorkspace};
use predicates::str::contains;

/// Segredo de teste para HMAC (≥32 chars, regra do jwt.rs). Valor FAKE_,
/// usado apenas nos processos filhos destes testes.
const FAKE_SECRET: &str = "FAKE_secret-para-testes-e2e-com-32-chars!!";

/// Plano mock que o Planner desserializa (mesmo formato do integration.rs).
fn mock_plan() -> &'static str {
    r#"{"goal":"criar hello.rs","non_goals":[],"constraints":[],"milestones":[{"id":"m1","title":"Criar hello.rs","description":"Criar arquivo hello.rs com fn main","acceptance_criteria":["compila"],"validation_cmd":"echo ok","decision_notes":[]}]}"#
}

// ═══════════════════════════════════════════════════════════════════════════════
// R-F-058: arreio commission
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn commission_cli_gera_artefatos_e_detecta_stub() {
    let ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();

    // Código com stub de alta severidade: "incompleto oculto não pode".
    ws.write_file("src_alvo/pendente.rs", "fn depois() { todo!() }\n");
    // Evidência primária: saída REAL de cargo test (formato verbatim).
    ws.write_file(
        "evidencia/testes.txt",
        "test result: ok. 1820 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );

    let mut cmd = ws.arreio_cmd();
    cmd.arg("commission")
        .arg("--src")
        .arg("src_alvo")
        .arg("--out")
        .arg("saida_pvc")
        .arg("--test-output")
        .arg("evidencia/testes.txt");
    cmd.assert()
        .success()
        .stdout(contains("Aprovado com restrições"));

    assert!(
        ws.path()
            .join("saida_pvc")
            .join("COMMISSIONING_REPORT.generated.md")
            .exists(),
        "relatório .generated deveria existir (promoção é HITL)"
    );

    // Sem nenhuma evidência: rejeitado (relatório sem evidência viola a regra PVC).
    let mut sem_evidencia = ws.arreio_cmd();
    sem_evidencia
        .arg("commission")
        .arg("--src")
        .arg("src_alvo")
        .arg("--out")
        .arg("saida_pvc2");
    sem_evidencia
        .assert()
        .failure()
        .stderr(contains("evidência"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// R-F-059/060: arreio credential + zero-trust no run
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn credential_cli_issue_verify_roundtrip() {
    let ws = ArreioWorkspace::new();

    // Sem ARREIO_JWT_SECRET: erro claro, nada emitido.
    let mut sem_secret = ws.arreio_cmd();
    sem_secret.env_remove("ARREIO_JWT_SECRET");
    sem_secret
        .arg("credential")
        .arg("issue")
        .arg("--agent-id")
        .arg("agent-e2e")
        .arg("--scope")
        .arg("tool:read_file");
    sem_secret
        .assert()
        .failure()
        .stderr(contains("ARREIO_JWT_SECRET"));

    // Emissão: stdout contém APENAS o token (pipeável), nunca o segredo.
    let mut issue = ws.arreio_cmd();
    issue.env("ARREIO_JWT_SECRET", FAKE_SECRET);
    issue
        .arg("credential")
        .arg("issue")
        .arg("--agent-id")
        .arg("agent-e2e")
        .arg("--role")
        .arg("developer")
        .arg("--scope")
        .arg("tool:read_file")
        .arg("--scope")
        .arg("vault:read:openai*")
        .arg("--ttl-hours")
        .arg("24");
    let out = issue.output().expect("rodar issue");
    assert!(out.status.success(), "issue deveria ter sucesso");
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        token.split('.').count(),
        3,
        "stdout deveria conter apenas o JWT (header.payload.sig)"
    );
    assert!(
        !token.contains(FAKE_SECRET),
        "segredo jamais pode aparecer na saída"
    );

    // Verificação: imprime claims, nunca o segredo.
    let mut verify = ws.arreio_cmd();
    verify.env("ARREIO_JWT_SECRET", FAKE_SECRET);
    verify.arg("credential").arg("verify").arg(&token);
    verify
        .assert()
        .success()
        .stdout(contains("agent-e2e"))
        .stdout(contains("developer"))
        .stdout(contains("tool:read_file"));

    // Segredo errado: assinatura inválida.
    let mut wrong = ws.arreio_cmd();
    wrong.env("ARREIO_JWT_SECRET", "FAKE_outro-segredo-de-32-caracteres!!!");
    wrong.arg("credential").arg("verify").arg(&token);
    wrong.assert().failure();
}

#[test]
fn run_com_credencial_expirada_falha_no_startup() {
    let mut ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();
    ws.write_spec("hello.spec", hello_world_spec());

    // ttl 0: exp == iat → expirada imediatamente (is_expired: now >= exp).
    let mut issue = ws.arreio_cmd();
    issue.env("ARREIO_JWT_SECRET", FAKE_SECRET);
    issue
        .arg("credential")
        .arg("issue")
        .arg("--agent-id")
        .arg("agent-exp")
        .arg("--scope")
        .arg("tool:*")
        .arg("--ttl-hours")
        .arg("0");
    let out = issue.output().expect("rodar issue");
    assert!(out.status.success());
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // O run deve falhar ANTES de qualquer planejamento/execução.
    let mut run = ws.arreio_cmd();
    run.env("ARREIO_JWT_SECRET", FAKE_SECRET);
    run.arg("run")
        .arg("hello.spec")
        .arg("--model")
        .arg(format!("mock:{}", mock_plan()))
        .arg("--recovery-strategy")
        .arg("none")
        .arg("--agent-credential")
        .arg(&token);
    run.assert().failure().stderr(contains("agent-credential"));

    // Nada foi planejado: DAG permanece vazio.
    let dag = ws.load_dag();
    assert!(
        dag.nodes().is_empty(),
        "credencial expirada não pode deixar execução começar"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// R-F-061/062: arreio reason + --reasoning-mode
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn reason_cli_gera_ledger_auditavel_com_mock() {
    let ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();

    let mut cmd = ws.arreio_cmd();
    cmd.arg("reason")
        .arg("qual é a resposta")
        .arg("--mode")
        .arg("cot")
        .arg("--model")
        .arg("mock:Step 1: pensar\nANSWER: 42")
        .arg("--session-id")
        .arg("s1");
    cmd.assert()
        .success()
        .stdout(contains("42"))
        .stdout(contains("chain_valid=true"));

    // Ledger persistido no Blackboard: tuplas reasoning::steps:s1:* + budget.
    let raw = std::fs::read_to_string(&ws.blackboard_path).expect("ler blackboard");
    assert!(
        raw.contains("steps:s1"),
        "passos do ledger da sessão s1 deveriam estar no Blackboard"
    );
    assert!(
        raw.contains("budget:s1"),
        "budget consumido da sessão s1 deveria estar auditado"
    );

    // Modo inválido: erro amigável com os modos aceitos.
    let mut invalido = ws.arreio_cmd();
    invalido
        .arg("reason")
        .arg("x")
        .arg("--mode")
        .arg("xyz")
        .arg("--model")
        .arg("mock:ANSWER: nunca");
    invalido.assert().failure().stderr(contains("modo de raciocínio inválido"));
}

#[test]
fn run_persiste_reasoning_mode_quando_flag_passada() {
    let mut ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();
    ws.write_spec("hello.spec", hello_world_spec());

    // run COM a flag: tupla reasoning::mode persistida com o nome canônico.
    let mut com_flag = ws.arreio_cmd();
    com_flag
        .arg("run")
        .arg("hello.spec")
        .arg("--model")
        .arg(format!("mock:{}", mock_plan()))
        .arg("--recovery-strategy")
        .arg("none")
        .arg("--reasoning-mode")
        .arg("cot");
    let _ = com_flag.output(); // o loop pode falhar com mock; a tupla é persistida antes

    let bb = arreio_kernel::Blackboard::open(&ws.blackboard_path).expect("abrir blackboard");
    assert_eq!(
        bb.get_tuple("reasoning", "mode"),
        Some(serde_json::json!("chain_of_thought")),
        "tupla reasoning::mode deveria ter o nome canônico do modo"
    );
    drop(bb);

    // run SEM a flag: tupla limpa (pipeline novo = default limpo).
    let mut sem_flag = ws.arreio_cmd();
    sem_flag
        .arg("run")
        .arg("hello.spec")
        .arg("--model")
        .arg(format!("mock:{}", mock_plan()))
        .arg("--recovery-strategy")
        .arg("none");
    let _ = sem_flag.output();

    let bb = arreio_kernel::Blackboard::open(&ws.blackboard_path).expect("abrir blackboard");
    assert_eq!(
        bb.get_tuple("reasoning", "mode"),
        None,
        "run sem --reasoning-mode deveria limpar o modo persistido"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// R-F-067/068/069: Execução PAL Sandboxed (PVC-Q4.3)
// ═══════════════════════════════════════════════════════════════════════════════

/// Resposta mock do modo program_aided: bloco ```program``` + ANSWER.
fn mock_pal(program: &str) -> String {
    format!(
        "mock:```program\n{}\n```\nANSWER: PROGRAM_PENDING_EXECUTION",
        program
    )
}

#[test]
fn reason_pal_sem_execute_nao_executa() {
    let ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();

    let mut cmd = ws.arreio_cmd();
    cmd.arg("reason")
        .arg("calcule algo")
        .arg("--mode")
        .arg("pal")
        .arg("--model")
        .arg(mock_pal("echo NAO_DEVE_RODAR"))
        .arg("--session-id")
        .arg("pal-default");
    // Default intocado: programa impresso, aviso de pendência, NADA executado.
    cmd.assert()
        .success()
        .stdout(contains("echo NAO_DEVE_RODAR"))
        .stderr(contains("PROGRAM_PENDING_EXECUTION"));
    assert!(
        !ws.path().join(".arreio").join("pal").exists(),
        "sem --execute-program nenhum arquivo de programa pode ser criado"
    );

    // --execute-program sem --program-runner: erro ANTES de chamar o LLM.
    let mut sem_runner = ws.arreio_cmd();
    sem_runner
        .arg("reason")
        .arg("x")
        .arg("--mode")
        .arg("pal")
        .arg("--model")
        .arg(mock_pal("echo x"))
        .arg("--execute-program");
    sem_runner
        .assert()
        .failure()
        .stderr(contains("--program-runner"));
}

#[test]
fn reason_pal_executa_programa_em_sandbox() {
    let ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();

    let mut cmd = ws.arreio_cmd();
    cmd.arg("reason")
        .arg("imprima o marcador")
        .arg("--mode")
        .arg("pal")
        .arg("--model")
        .arg(mock_pal("@echo off\necho PAL_EXEC_OK"))
        .arg("--session-id")
        .arg("pal-exec")
        .arg("--execute-program")
        .arg("--program-runner")
        .arg("cmd /c")
        .arg("--program-ext")
        .arg("cmd");
    cmd.assert()
        .success()
        .stdout(contains("PAL_EXEC_OK"))
        .stdout(contains("exit=0"))
        .stdout(contains("chain_valid=true"));

    // Arquivo confinado no workspace + execução auditada no ledger.
    assert!(ws
        .path()
        .join(".arreio")
        .join("pal")
        .join("pal-exec.cmd")
        .exists());
    let raw = std::fs::read_to_string(&ws.blackboard_path).expect("ler blackboard");
    assert!(
        raw.contains("pal_execution"),
        "a execução deve virar passo Observation no ledger hash-chain"
    );
}

#[test]
fn reason_pal_bloqueia_programa_perigoso() {
    let ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();

    let mut cmd = ws.arreio_cmd();
    cmd.arg("reason")
        .arg("limpe o disco")
        .arg("--mode")
        .arg("pal")
        .arg("--model")
        .arg(mock_pal("rm -rf /tmp/alvo"))
        .arg("--session-id")
        .arg("pal-mal")
        .arg("--execute-program")
        .arg("--program-runner")
        .arg("cmd /c");
    // Scan de conteúdo (L1) bloqueia ANTES do disco: exit ≠ 0 + auditoria.
    cmd.assert().failure().stderr(contains("PROGRAM_BLOCKED"));
    assert!(
        !ws.path().join(".arreio").join("pal").join("pal-mal.prog").exists(),
        "programa bloqueado nunca chega ao disco"
    );
    let raw = std::fs::read_to_string(&ws.blackboard_path).expect("ler blackboard");
    assert!(
        raw.contains("pal_execution_blocked"),
        "o bloqueio deve ser auditado como Observation no ledger"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// R-F-063: arreio score + status com score
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn score_cli_define_score_e_status_exibe() {
    let mut ws = ArreioWorkspace::new();
    ws.arreio_cmd().arg("init").assert().success();
    ws.write_spec("hello.spec", hello_world_spec());

    // Cria o DAG (m1) via run com mock — pode falhar no loop, o DAG persiste.
    let mut run = ws.arreio_cmd();
    run.arg("run")
        .arg("hello.spec")
        .arg("--model")
        .arg(format!("mock:{}", mock_plan()))
        .arg("--recovery-strategy")
        .arg("none");
    let _ = run.output();
    assert!(!ws.load_dag().nodes().is_empty(), "DAG deveria existir");

    // score set em nó real: tupla dag::score:m1 via mecanismo do arreio-dag.
    let mut set = ws.arreio_cmd();
    set.arg("score")
        .arg("set")
        .arg("m1")
        .arg("--urgency")
        .arg("1.0")
        .arg("--importance")
        .arg("0.9");
    set.assert()
        .success()
        .stdout(contains("score de 'm1' definido"));

    // score list e status exibem o composto.
    let mut list = ws.arreio_cmd();
    list.arg("score").arg("list");
    list.assert()
        .success()
        .stdout(contains("m1"))
        .stdout(contains("score="));

    let mut status = ws.arreio_cmd();
    status.arg("status");
    status.assert().success().stdout(contains("[score "));

    // Nó inexistente: erro do arreio-dag propagado (exit ≠ 0).
    let mut fantasma = ws.arreio_cmd();
    fantasma
        .arg("score")
        .arg("set")
        .arg("fantasma")
        .arg("--urgency")
        .arg("0.7");
    fantasma.assert().failure();
}
