//! Arreio-Bridge-OpenClaw — Adapter para OpenClaw.
//!
//! REST client para Gateway OpenClaw.
//! Importa `.openclaw` config e claws para Skill Store.
//! Exporta tuplas Blackboard como "memórias" no OpenClaw.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;

// ── Cliente REST ──────────────────────────────────────────────────────────────

/// Cliente REST síncrono para o Gateway OpenClaw.
pub struct OpenClawClient {
    gateway_url: String,
}

impl OpenClawClient {
    pub fn new(gateway_url: String) -> Self {
        Self { gateway_url }
    }

    /// Envia mensagem para sessão OpenClaw via POST /api/sessions/{session}/messages
    pub fn send_message(&self, session: &str, message: &str) -> Result<String> {
        let (host, port) = parse_url(&self.gateway_url)?;
        let path = format!("/api/sessions/{}/messages", session);
        let body = serde_json::json!({ "message": message }).to_string();
        http_request("POST", &host, port, &path, Some(&body))
    }

    /// Lista cron jobs: GET /api/cron
    pub fn list_cron_jobs(&self) -> Result<Vec<String>> {
        let (host, port) = parse_url(&self.gateway_url)?;
        let body = http_request("GET", &host, port, "/api/cron", None)?;
        let jobs: Vec<String> = serde_json::from_str(&body)
            .with_context(|| format!("JSON inválido em list_cron_jobs: {}", body))?;
        Ok(jobs)
    }

    /// Obtém histórico: GET /api/sessions/{session}/history
    pub fn get_history(&self, session: &str) -> Result<String> {
        let (host, port) = parse_url(&self.gateway_url)?;
        let path = format!("/api/sessions/{}/history", session);
        http_request("GET", &host, port, &path, None)
    }
}

// ── Importador de Config ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OpenClawConfig {
    #[serde(default)]
    claws: Vec<ClawEntry>,
}

#[derive(Debug, Deserialize)]
struct ClawEntry {
    name: String,
    description: String,
    #[serde(default)]
    patterns: Vec<String>,
    instruction: String,
    #[serde(default)]
    steps: Vec<String>,
    #[serde(default)]
    templates: HashMap<String, String>,
    #[serde(default)]
    validation_cmds: Vec<String>,
}

/// Importa `.openclaw` config e claws para Skill Store.
pub fn import_openclaw_config(
    path: &str,
    skills: &mut arreio_skills::SkillStore,
) -> Result<Vec<String>> {
    let path = Path::new(path);
    let config_path = if path.is_dir() {
        path.join("openclaw.json")
    } else {
        path.to_path_buf()
    };
    if !config_path.exists() {
        bail!(
            "arquivo de configuração não encontrado: {}",
            config_path.display()
        );
    }
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("lendo {}", config_path.display()))?;
    let config: OpenClawConfig = serde_json::from_str(&raw)
        .with_context(|| format!("parse JSON em {}", config_path.display()))?;

    let mut imported = Vec::new();
    for claw in config.claws {
        let skill = arreio_skills::Skill {
            name: claw.name.clone(),
            description: claw.description,
            trigger_patterns: claw.patterns,
            ast_signature: None,
            file_target_pattern: None,
            instruction_template: claw.instruction,
            steps: claw.steps,
            templates: claw.templates,
            validation_cmds: claw.validation_cmds,
            last_used: 0,
            usage_count: 0,
            success_rate: 0.0,
            created_from_dag_task_id: None,
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: arreio_skills::SkillTrust::Untrusted,
            module_count: 1,
            mutation_history: vec![],
        };
        skills.save(&skill)?;
        imported.push(claw.name);
    }
    Ok(imported)
}

// ── Exportador de Memória ────────────────────────────────────────────────────

/// Exporta tuplas Blackboard como memórias OpenClaw.
pub fn export_to_openclaw(
    blackboard: &arreio_kernel::Blackboard,
    client: &OpenClawClient,
    session: &str,
) -> Result<()> {
    let tuples = blackboard.list_tuples();
    let payload = serde_json::json!({
        "source": "arreio",
        "tuples": tuples.iter().map(|(cat, key, val)| {
            serde_json::json!({
                "category": cat,
                "key": key,
                "value": val,
            })
        }).collect::<Vec<_>>()
    });
    let body = payload.to_string();
    client.send_message(session, &body)?;
    Ok(())
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

fn parse_url(url: &str) -> Result<(String, u16)> {
    let without_scheme = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    let (host, port_str) = host_port.split_once(':').unwrap_or((host_port, "80"));
    let port = port_str.parse::<u16>()?;
    Ok((host.to_string(), port))
}

fn http_request(
    method: &str,
    host: &str,
    port: u16,
    path: &str,
    body: Option<&str>,
) -> Result<String> {
    let request = if let Some(b) = body {
        format!(
            "{} {} HTTP/1.0\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            method, path, host, port, b.len(), b
        )
    } else {
        format!(
            "{} {} HTTP/1.0\r\nHost: {}:{}\r\n\r\n",
            method, path, host, port
        )
    };

    let mut stream = TcpStream::connect((host, port))
        .with_context(|| format!("falha ao conectar a {}:{}", host, port))?;
    stream
        .write_all(request.as_bytes())
        .context("falha ao enviar request HTTP")?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .context("falha ao ler resposta HTTP")?;

    let (status, response_body) = parse_http_response(&raw)?;
    if status < 200 || status >= 300 {
        bail!("HTTP {}: {}", status, response_body);
    }
    Ok(response_body)
}

fn parse_http_response(raw: &str) -> Result<(u16, String)> {
    let mut lines = raw.lines();
    let status_line = lines.next().context("resposta HTTP vazia")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .context("status HTTP inválido")?;

    // Pula headers até linha em branco
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
    }

    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((status, body))
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use arreio_skills::SkillStore;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    /// Inicia servidor TCP mínimo que responde com `response` e fecha a conexão.
    fn mock_server(response: String) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf); // lê request (suficiente para testes)
            let _ = stream.write_all(response.as_bytes());
            // stream fecha ao sair do escopo
        });
        (format!("http://127.0.0.1:{}", port), handle)
    }

    /// Inicia servidor TCP mínimo e devolve o request recebido via canal.
    fn mock_server_with_capture(
        response: String,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = tx.send(req);
            let _ = stream.write_all(response.as_bytes());
        });
        (format!("http://127.0.0.1:{}", port), rx, handle)
    }

    fn temp_blackboard() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    #[test]
    fn client_new() {
        let c = OpenClawClient::new("http://127.0.0.1:18789".to_string());
        assert_eq!(c.gateway_url, "http://127.0.0.1:18789");
    }

    #[test]
    fn send_message_ok() {
        let body = r#"{"id":"msg1","content":"ok"}"#;
        let resp = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (url, _handle) = mock_server(resp);
        let client = OpenClawClient::new(url);
        let result = client.send_message("sess-1", "hello").unwrap();
        assert!(result.contains("msg1"));
    }

    #[test]
    fn send_message_http_error() {
        let resp = "HTTP/1.0 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nerror";
        let (url, _handle) = mock_server(resp.to_string());
        let client = OpenClawClient::new(url);
        assert!(client.send_message("sess-1", "hello").is_err());
    }

    #[test]
    fn list_cron_jobs_ok() {
        let body = r#"["backup","cleanup"]"#;
        let resp = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (url, _handle) = mock_server(resp);
        let client = OpenClawClient::new(url);
        let jobs = client.list_cron_jobs().unwrap();
        assert_eq!(jobs, vec!["backup", "cleanup"]);
    }

    #[test]
    fn get_history_ok() {
        let body = r#"{"messages":["m1","m2"]}"#;
        let resp = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (url, _handle) = mock_server(resp);
        let client = OpenClawClient::new(url);
        let hist = client.get_history("sess-1").unwrap();
        assert!(hist.contains("m1"));
    }

    #[test]
    fn import_config_file_ok() {
        let raw = r#"{
            "claws": [
                {
                    "name": "rust-fmt",
                    "description": "Formata código Rust",
                    "patterns": ["fmt","format"],
                    "instruction": "Execute cargo fmt"
                }
            ]
        }"#;
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        fs::write(path, raw).unwrap();
        let bb = temp_blackboard();
        let mut store = SkillStore::new(bb);
        let imported = import_openclaw_config(path, &mut store).unwrap();
        assert_eq!(imported, vec!["rust-fmt"]);
        let skill = store.get("rust-fmt").unwrap();
        assert_eq!(skill.description, "Formata código Rust");
    }

    #[test]
    fn import_config_dir_ok() {
        let dir = tempfile::tempdir().unwrap();
        let raw = r#"{
            "claws": [
                {
                    "name": "claw-test",
                    "description": "Teste",
                    "patterns": [],
                    "instruction": "echo test"
                }
            ]
        }"#;
        fs::write(dir.path().join("openclaw.json"), raw).unwrap();
        let bb = temp_blackboard();
        let mut store = SkillStore::new(bb);
        let imported = import_openclaw_config(dir.path().to_str().unwrap(), &mut store).unwrap();
        assert_eq!(imported, vec!["claw-test"]);
    }

    #[test]
    fn import_config_missing_file() {
        let bb = temp_blackboard();
        let mut store = SkillStore::new(bb);
        let result = import_openclaw_config("/caminho/inexistente/openclaw.json", &mut store);
        assert!(result.is_err());
    }

    #[test]
    fn import_config_empty_claws() {
        let raw = r#"{"claws":[]}"#;
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        fs::write(path, raw).unwrap();
        let bb = temp_blackboard();
        let mut store = SkillStore::new(bb);
        let imported = import_openclaw_config(path, &mut store).unwrap();
        assert!(imported.is_empty());
    }

    #[test]
    fn export_to_openclaw_ok() {
        let bb = temp_blackboard();
        bb.put_tuple("fsm", "state", serde_json::json!("IDLE"))
            .unwrap();
        bb.put_tuple("task", "t1", serde_json::json!({"ok": true}))
            .unwrap();

        let body = r#"{"status":"received"}"#;
        let resp = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (url, rx, _handle) = mock_server_with_capture(resp);
        let client = OpenClawClient::new(url);
        export_to_openclaw(&bb, &client, "session-x").unwrap();

        let req = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(req.contains("POST /api/sessions/session-x/messages"));
        assert!(req.contains("arreio"));
        assert!(req.contains("fsm"));
        assert!(req.contains("task"));
    }

    #[test]
    fn export_to_openclaw_empty() {
        let bb = temp_blackboard();
        let body = r#"{"status":"received"}"#;
        let resp = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (url, _handle) = mock_server(resp);
        let client = OpenClawClient::new(url);
        export_to_openclaw(&bb, &client, "session-y").unwrap();
    }
}
