pub mod auth;
pub mod session_store;
pub mod stream_consumer;

use anyhow::{Context, Result};
use auth::{AuthConfig, AuthContext, AuthMiddleware, AuthMode};
use arreio_dag::Dag;
use arreio_fsm::Fsm;
use arreio_kernel::{Blackboard, DEFAULT_MODEL_STR};
use arreio_provider::ProviderClient;
use arreio_scheduler::{JobSchedule, JobStatus, ArreioScheduler, ScheduledJob};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

pub use session_store::{Session, SessionStore};
pub use stream_consumer::{StreamConsumer, StreamEvent};

const DASHBOARD_HTML: &str = include_str!("../assets/dashboard.html");

/// Servidor HTTP síncrono embutido — gateway persistente do Arreio.
pub struct GatewayServer {
    blackboard: Blackboard,
    addr: String,
    auth: AuthMiddleware,
}

impl GatewayServer {
    pub fn new(blackboard: Blackboard, port: u16) -> Self {
        let auth_mode_str = std::env::var("ARREIO_AUTH_MODE").unwrap_or_default();
        if auth_mode_str.is_empty() {
            eprintln!("[gateway] AVISO: ARREIO_AUTH_MODE não definida — usando NoAuth (todas as rotas liberadas)");
        }
        let auth_config = AuthConfig::new(AuthMode::from_str(&auth_mode_str));
        let auth = AuthMiddleware::new(auth_config, blackboard.clone());
        Self {
            blackboard,
            addr: format!("127.0.0.1:{}", port),
            auth,
        }
    }

    /// Configura o modo de autenticação explicitamente (ignora env ARREIO_AUTH_MODE).
    pub fn with_auth_mode(mut self, mode: AuthMode) -> Self {
        let config = AuthConfig::new(mode);
        self.auth = AuthMiddleware::new(config, self.blackboard.clone());
        self
    }

    pub fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.addr)
            .with_context(|| format!("falha ao bind em {}", self.addr))?;
        let auth_mode = self.auth.mode();
        println!("[gateway] ouvindo em http://{} (auth={})", self.addr, auth_mode.as_str());

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let bb = self.blackboard.clone();
                    let auth = self.auth.clone_light();
                    thread::spawn(move || {
                        let _ = handle_connection(stream, bb, auth);
                    });
                }
                Err(e) => eprintln!("[gateway] erro de conexão: {}", e),
            }
        }
        Ok(())
    }
}

fn handle_connection(mut stream: TcpStream, bb: Blackboard, auth: AuthMiddleware) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(&stream);

    // Lê a primeira linha: METHOD PATH HTTP/1.1
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let parts: Vec<&str> = first_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        return send_response(&mut stream, 400, "text/plain", "Bad Request");
    }
    let method = parts[0];
    let path = parts[1];

    // Lê headers até linha em branco
    let mut headers_raw: Vec<(String, String)> = Vec::new();
    let mut body = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        // Parse header line
        if let Some(colon) = line.find(':') {
            let name = line[..colon].trim().to_string();
            let value = line[colon + 1..].trim().to_string();
            // Content-Length: ler body
            if name.to_lowercase() == "content-length" {
                if let Ok(len) = value.trim().parse::<usize>() {
                    if len > 0 {
                        let mut buf = vec![0u8; len];
                        reader.read_exact(&mut buf)?;
                        body = String::from_utf8_lossy(&buf).into_owned();
                    }
                }
            }
            headers_raw.push((name, value));
        } else {
            headers_raw.push((line.trim().to_string(), String::new()));
        }
    }

    // ── Autenticação ────────────────────────────────────────────────────
    if !auth.is_public(path) {
        match auth.authenticate(&headers_raw, path) {
            Some(_ctx) => {
                // Autenticado — ctx disponível para RBAC futuro
            }
            None => {
                return send_response(
                    &mut stream,
                    401,
                    "application/json",
                    r#"{"error":"unauthorized","message":"Token inválido ou ausente. Use X-API-Key ou Authorization: Bearer"}"#,
                );
            }
        }
    }

    // Capture auth context para handlers que precisam de RBAC
    let auth_ctx = if auth.is_public(path) {
        Some(AuthContext {
            client_id: "anonymous".into(),
            role: "admin".into(),
            auth_method: "noauth".into(),
        })
    } else {
        auth.authenticate(&headers_raw, path)
    };

    match (method, path) {
        ("GET", "/") => send_response(&mut stream, 200, "text/html", DASHBOARD_HTML),
        ("GET", "/api/status") => api_status(&mut stream, bb),
        ("POST", "/api/run") => api_run(&mut stream, bb, &body),
        ("POST", "/api/resume") => api_resume(&mut stream, bb, &body),
        ("POST", "/api/rollback") => api_rollback(&mut stream, bb),
        ("GET", "/api/skills") => api_skills(&mut stream, bb),
        ("GET", "/api/events") => api_events(&mut stream, bb),
        ("GET", "/api/todos") => api_todos(&mut stream, bb),
        ("GET", "/api/schedule") => api_schedule_list(&mut stream, bb),
        ("POST", "/api/schedule") => api_schedule_create(&mut stream, bb, &body),
        ("POST", "/v1/chat/completions") => api_chat_completions(&mut stream, bb, &body),
        ("POST", "/api/chat") => api_chat(&mut stream, bb, &body),
        ("POST", "/api/chat/stream") => api_chat_stream(&mut stream, bb, &body),
        ("GET", "/api/sessions") => api_session_list(&mut stream, bb),
        ("GET", path) if path.starts_with("/api/session/") => {
            let id = path.strip_prefix("/api/session/").unwrap_or("");
            api_session_get(&mut stream, bb, id)
        }
        // PVC-Q1.2: HITL endpoints
        ("GET", "/api/pending-approvals") => {
            api_pending_approvals(&mut stream, bb, &auth, auth_ctx.as_ref())
        }
        ("POST", path) if path.starts_with("/api/approve/") => {
            let id = path.strip_prefix("/api/approve/").unwrap_or("");
            api_approve(&mut stream, bb, &auth, auth_ctx.as_ref(), id, &body)
        }
        ("POST", path) if path.starts_with("/api/reject/") => {
            let id = path.strip_prefix("/api/reject/").unwrap_or("");
            api_reject(&mut stream, bb, &auth, auth_ctx.as_ref(), id, &body)
        }
        ("GET", path) if path.starts_with("/api/approvals/") => {
            let id = path.strip_prefix("/api/approvals/").unwrap_or("");
            api_approval_status(&mut stream, bb, id)
        }
        ("GET", "/metrics") => api_metrics(&mut stream, bb),
        ("GET", "/health") => api_health(&mut stream, bb),
        ("GET", "/health/otel") => api_health_otel(&mut stream),
        ("POST", "/api/auth/login") => api_auth_login(&mut stream, &auth, &body),
        ("DELETE", path) if path.starts_with("/api/schedule/") => {
            let id = path.strip_prefix("/api/schedule/").unwrap_or("");
            api_schedule_delete(&mut stream, bb, id)
        }
        _ => send_response(
            &mut stream,
            404,
            "application/json",
            r#"{"error":"not found"}"#,
        ),
    }
}

// ── Rotas da API ──────────────────────────────────────────────────────────────

fn api_status(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let fsm = Fsm::new(bb.clone());
    let dag = Dag::load(bb.clone()).unwrap_or_else(|_| Dag::new(vec![], bb.clone()).unwrap());
    let s = dag.summary();

    let json = serde_json::json!({
        "fsm": fsm.current().to_string(),
        "dag": {
            "todo": s.todo,
            "doing": s.doing,
            "done": s.done,
            "failed": s.failed,
            "total": s.total,
            "nodes": dag.nodes(),
        }
    });
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_run(stream: &mut TcpStream, _bb: Blackboard, body: &str) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };
    let spec = payload.get("spec").and_then(|v| v.as_str()).unwrap_or("");
    if spec.is_empty() {
        return send_response(
            stream,
            400,
            "application/json",
            r#"{"error":"missing spec"}"#,
        );
    }

    // Nota: execução síncrona do pipeline SYMBION via API não está implementada.
    // Use o CLI: arreio run <spec> ou o scheduler com arreio serve.
    send_response(
        stream,
        501,
        "application/json",
        &format!(
            r#"{{"error":"Execução síncrona não implementada. Use CLI: arreio run {}","spec":"{}"}}"#,
            spec, spec
        ),
    )
}

fn api_resume(stream: &mut TcpStream, bb: Blackboard, body: &str) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL_STR);

    let scheduler = ArreioScheduler::new(bb);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let job = ScheduledJob {
        id: format!("api-resume-{}", now),
        name: "api-resume".into(),
        description: format!("API resume (model={})", model),
        schedule: JobSchedule::IntervalMinutes(0),
        status: JobStatus::Pending,
        command: "__RESUME__".into(), // marcador especial: o scheduler deve tratar como resume
        last_run: None,
        next_run: now,
        created_at: now,
        run_count: 0,
    };
    match scheduler.schedule(job.clone()) {
        Ok(_) => {
            let json = serde_json::json!({"status": "scheduled", "job": job});
            send_response(stream, 202, "application/json", &json.to_string())
        }
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

fn api_rollback(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    match arreio_dag::Checkpoint::rollback(
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .as_path(),
    ) {
        Ok(_) => {
            let _ = bb.put_tuple("system", "rollback", serde_json::json!({"timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}));
            send_response(
                stream,
                200,
                "application/json",
                r#"{"status":"rollback_completed"}"#,
            )
        }
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

fn api_skills(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let skills = bb.search_tuples("skills", "");
    let mut arr = Vec::new();
    for (k, v) in skills {
        // Tenta extrair trust_level do Skill serializado
        let trust = v
            .get("trust_level")
            .and_then(|t| t.as_str())
            .unwrap_or("Untrusted");
        let module_count = v.get("module_count").and_then(|m| m.as_u64()).unwrap_or(1);
        arr.push(serde_json::json!({
            "name": k,
            "trust_level": trust,
            "module_count": module_count,
            "data": v,
        }));
    }
    let json = serde_json::json!({"skills": arr});
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_todos(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let store = arreio_dag::TodoStore::new(bb, "default-session");
    let items = store.list();
    let (pending, in_progress, completed, cancelled, total) = store.kanban_summary();
    let json = serde_json::json!({
        "todos": items,
        "summary": {
            "pending": pending,
            "in_progress": in_progress,
            "completed": completed,
            "cancelled": cancelled,
            "total": total,
        }
    });
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_schedule_list(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let scheduler = ArreioScheduler::new(bb);
    let jobs = scheduler.list();
    let json = serde_json::json!({"jobs": jobs});
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_schedule_create(stream: &mut TcpStream, bb: Blackboard, body: &str) -> Result<()> {
    let scheduler = ArreioScheduler::new(bb);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };

    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unnamed");
    let command = payload
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let interval = payload
        .get("interval_minutes")
        .and_then(|v| v.as_u64())
        .unwrap_or(60) as u32;

    let job = ScheduledJob {
        id: format!("job-{}", now),
        name: name.into(),
        description: format!("API: {}", command),
        schedule: JobSchedule::IntervalMinutes(interval),
        status: JobStatus::Pending,
        command: command.into(),
        last_run: None,
        next_run: now,
        created_at: now,
        run_count: 0,
    };

    match scheduler.schedule(job.clone()) {
        Ok(_) => {
            let json = serde_json::json!({"status": "created", "job": job});
            send_response(stream, 201, "application/json", &json.to_string())
        }
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

fn api_schedule_delete(stream: &mut TcpStream, bb: Blackboard, id: &str) -> Result<()> {
    let scheduler = ArreioScheduler::new(bb);
    match scheduler.remove(id) {
        Ok(_) => send_response(stream, 200, "application/json", r#"{"status":"removed"}"#),
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

/// Extrai system prompt e user message de um payload OpenAI.
fn parse_openai_messages(payload: &Value) -> (String, String) {
    let messages = payload
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut system = String::new();
    let mut user_parts = Vec::new();
    for msg in &messages {
        if let Some(role) = msg.get("role").and_then(|v| v.as_str()) {
            if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                match role {
                    "system" => system = content.to_string(),
                    "user" => user_parts.push(content.to_string()),
                    "assistant" => {} // stateless: ignora histórico
                    _ => {}
                }
            }
        }
    }
    (system, user_parts.join("\n\n"))
}

/// API OpenAI-compatible: POST /v1/chat/completions
fn api_chat_completions(stream: &mut TcpStream, bb: Blackboard, body: &str) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };

    let stream_flag = payload
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL_STR);
    let (system, user) = parse_openai_messages(&payload);

    let req = arreio_provider::ChatRequest {
        messages: Vec::new(),
        model: model.to_string(),
        system,
        user,
        tools: None,
    };

    let provider = arreio_provider::OllamaProvider::new(bb);

    if stream_flag {
        return api_chat_completions_stream(stream, &provider, &req, model);
    }

    match provider.chat(req) {
        Ok(response) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let json = serde_json::json!({
                "id": format!("chatcmpl-{}-arreio", now),
                "object": "chat.completion",
                "created": now,
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": response.content,
                        },
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": response.tokens_in,
                    "completion_tokens": response.tokens_out,
                    "total_tokens": response.tokens_in + response.tokens_out,
                }
            });
            send_response(stream, 200, "application/json", &json.to_string())
        }
        Err(e) => {
            let err_json = serde_json::json!({
                "error": {
                    "message": e.to_string(),
                    "type": "provider_error",
                    "code": null,
                }
            });
            send_response(stream, 502, "application/json", &err_json.to_string())
        }
    }
}

/// Streaming SSE para /v1/chat/completions.
fn api_chat_completions_stream(
    stream: &mut TcpStream,
    provider: &arreio_provider::OllamaProvider,
    req: &arreio_provider::ChatRequest,
    model: &str,
) -> Result<()> {
    let mut iter = match provider.chat_stream(req.clone()) {
        Ok(i) => i,
        Err(e) => {
            return send_response(
                stream,
                502,
                "application/json",
                &format!(r#"{{"error":"{}"}}"#, e),
            )
        }
    };

    let id = format!("chatcmpl-{}-arreio", now_epoch_secs());
    let headers = "HTTP/1.1 200 OK\r\n\
                   Content-Type: text/event-stream\r\n\
                   Cache-Control: no-cache\r\n\
                   Connection: keep-alive\r\n\r\n";
    stream.write_all(headers.as_bytes())?;
    stream.flush()?;

    let mut content_so_far = String::new();
    for chunk_result in &mut iter {
        match chunk_result {
            Ok(chunk) => {
                content_so_far.push_str(&chunk);
                let event = serde_json::json!({
                    "id": &id,
                    "object": "chat.completion.chunk",
                    "created": now_epoch_secs(),
                    "model": model,
                    "choices": [
                        {
                            "index": 0,
                            "delta": {
                                "role": "assistant",
                                "content": chunk,
                            },
                            "finish_reason": null,
                        }
                    ]
                });
                let sse = format!("data: {}\n\n", event.to_string());
                if stream.write_all(sse.as_bytes()).is_err() {
                    break; // cliente desconectou
                }
                if stream.flush().is_err() {
                    break;
                }
            }
            Err(e) => {
                let err_event = format!("data: {{\"error\":\"{}\"}}\n\n", e);
                let _ = stream.write_all(err_event.as_bytes());
                break;
            }
        }
    }

    // Evento final [DONE]
    let _ = stream.write_all(b"data: [DONE]\n\n");
    let _ = stream.flush();
    Ok(())
}

// ── Chat Transparente API ─────────────────────────────────────────────────────

fn api_chat(stream: &mut TcpStream, bb: Blackboard, body: &str) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };

    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL_STR);
    let session_id = payload.get("session_id").and_then(|v| v.as_str());

    if message.is_empty() {
        return send_response(
            stream,
            400,
            "application/json",
            r#"{"error":"missing message"}"#,
        );
    }

    let session_mgr = arreio_memory::TransparentSessionManager::new(bb.clone());
    let active = match session_id {
        Some(id) => match session_mgr.get_session(id) {
            Ok(Some(s)) => s,
            _ => session_mgr
                .get_or_create("gateway", model)
                .map(|a| a.session)
                .unwrap_or_else(|_| {
                    session_mgr
                        .create_session("gateway", model, arreio_memory::SessionMode::Conversational)
                        .unwrap()
                }),
        },
        None => session_mgr
            .get_or_create("gateway", model)
            .map(|a| a.session)
            .unwrap_or_else(|_| {
                session_mgr
                    .create_session("gateway", model, arreio_memory::SessionMode::Conversational)
                    .unwrap()
            }),
    };

    let provider = arreio_provider::OllamaProvider::new(bb.clone());
    let req = arreio_provider::ChatRequest {
        model: model.to_string(),
        system: "Você é um assistente amigável.".to_string(),
        user: message.to_string(),
        messages: Vec::new(),
        tools: None,
    };

    match provider.chat(req) {
        Ok(response) => {
            let _ = session_mgr.append_message(
                &active.id,
                arreio_memory::ChatRole::User,
                message,
                None,
                None,
                message.len() / 4,
            );
            let _ = session_mgr.append_message(
                &active.id,
                arreio_memory::ChatRole::Assistant,
                &response.content,
                None,
                None,
                response.content.len() / 4,
            );

            let json = serde_json::json!({
                "session_id": active.id,
                "response": response.content,
                "tokens_in": response.tokens_in,
                "tokens_out": response.tokens_out,
            });
            send_response(stream, 200, "application/json", &json.to_string())
        }
        Err(e) => send_response(
            stream,
            502,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

fn api_chat_stream(stream: &mut TcpStream, bb: Blackboard, body: &str) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };

    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_MODEL_STR);

    if message.is_empty() {
        return send_response(
            stream,
            400,
            "application/json",
            r#"{"error":"missing message"}"#,
        );
    }

    let provider = arreio_provider::OllamaProvider::new(bb);
    let req = arreio_provider::ChatRequest {
        model: model.to_string(),
        system: "Você é um assistente amigável.".to_string(),
        user: message.to_string(),
        messages: Vec::new(),
        tools: None,
    };

    let mut iter = match provider.chat_stream(req) {
        Ok(i) => i,
        Err(e) => {
            return send_response(
                stream,
                502,
                "application/json",
                &format!(r#"{{"error":"{}"}}"#, e),
            )
        }
    };

    let headers = "HTTP/1.1 200 OK\r\n\
                   Content-Type: text/event-stream\r\n\
                   Cache-Control: no-cache\r\n\
                   Connection: keep-alive\r\n\r\n";
    stream.write_all(headers.as_bytes())?;
    stream.flush()?;

    for chunk_result in &mut iter {
        match chunk_result {
            Ok(chunk) => {
                let sse = format!("data: {{\"chunk\":\"{}\"}}\n\n", chunk.replace('"', "\\\""));
                if stream.write_all(sse.as_bytes()).is_err() {
                    break;
                }
                if stream.flush().is_err() {
                    break;
                }
            }
            Err(e) => {
                let err_event = format!("data: {{\"error\":\"{}\"}}\n\n", e);
                let _ = stream.write_all(err_event.as_bytes());
                break;
            }
        }
    }

    let _ = stream.write_all(b"data: [DONE]\n\n");
    let _ = stream.flush();
    Ok(())
}

fn api_session_list(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let session_mgr = arreio_memory::TransparentSessionManager::new(bb);
    match session_mgr.list_sessions() {
        Ok(sessions) => {
            let json = serde_json::json!({"sessions": sessions});
            send_response(stream, 200, "application/json", &json.to_string())
        }
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

fn api_session_get(stream: &mut TcpStream, bb: Blackboard, id: &str) -> Result<()> {
    let session_mgr = arreio_memory::TransparentSessionManager::new(bb);
    match session_mgr.get_session(id) {
        Ok(Some(session)) => {
            let messages = session_mgr.list_messages(id).unwrap_or_default();
            let json = serde_json::json!({
                "session": session,
                "messages": messages,
            });
            send_response(stream, 200, "application/json", &json.to_string())
        }
        Ok(None) => send_response(
            stream,
            404,
            "application/json",
            r#"{"error":"session not found"}"#,
        ),
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn api_metrics(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let collector = arreio_telemetry::MetricsCollector::new(bb);
    let null_path = std::env::temp_dir().join("arreio_metrics_null.txt");
    let exporter =
        arreio_telemetry::OtlpJsonExporter::new(collector, null_path.to_str().unwrap_or("/dev/null"));
    let text = exporter.to_prometheus_text();
    send_response(stream, 200, "text/plain; version=0.0.4", &text)
}

fn api_health(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let collector = arreio_telemetry::MetricsCollector::new(bb.clone());
    // Correção: passar o caminho real de persistência do Blackboard (antes ia `None`,
    // que caía sempre no ramo "caminho não verificado" → status Degraded enganoso).
    // Agora a sonda é honesta: Healthy se o db existe, Unhealthy se o path existe mas o arquivo não.
    let results = arreio_telemetry::HealthProbe::check_all(Some(bb.store_path()), Some(&collector));
    let overall = arreio_telemetry::HealthProbe::aggregate(&results);
    let body = serde_json::json!({
        "status": match overall {
            arreio_telemetry::HealthStatus::Healthy => "healthy",
            arreio_telemetry::HealthStatus::Degraded => "degraded",
            arreio_telemetry::HealthStatus::Unhealthy => "unhealthy",
        },
        "subsystems": results.iter().map(|r| serde_json::json!({
            "name": r.name,
            "status": match r.status {
                arreio_telemetry::HealthStatus::Healthy => "healthy",
                arreio_telemetry::HealthStatus::Degraded => "degraded",
                arreio_telemetry::HealthStatus::Unhealthy => "unhealthy",
            },
            "message": r.message,
        })).collect::<Vec<_>>(),
    });
    send_response(
        stream,
        overall.http_code(),
        "application/json",
        &body.to_string(),
    )
}

/// Endpoint de health do OTLP: GET /health/otel
/// Retorna status da configuração do exportador OTLP.
fn api_health_otel(stream: &mut TcpStream) -> Result<()> {
    let endpoint = std::env::var("ARREIO_OTEL_ENDPOINT").unwrap_or_default();
    let configured = !endpoint.is_empty() && endpoint != "http://localhost:4318";
    let body = serde_json::json!({
        "otel": {
            "configured": configured,
            "endpoint": if configured { endpoint } else { "not configured".into() },
            "protocol": "otlp/http+json",
        }
    });
    send_response(stream, 200, "application/json", &body.to_string())
}

/// Endpoint de login: POST /api/auth/login
/// Body: { "password": "master-password" }
/// Response: { "token": "<jwt>" } ou 401
fn api_auth_login(stream: &mut TcpStream, auth: &AuthMiddleware, body: &str) -> Result<()> {
    let payload: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return send_response(
                stream,
                400,
                "application/json",
                r#"{"error":"invalid json"}"#,
            )
        }
    };

    let password = payload
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if password.is_empty() {
        return send_response(
            stream,
            400,
            "application/json",
            r#"{"error":"missing password"}"#,
        );
    }

    match auth.login(password) {
        Ok(Some(token)) => {
            let json = serde_json::json!({
                "token": token,
                "token_type": "bearer",
                "expires_in": 86400,
            });
            send_response(stream, 200, "application/json", &json.to_string())
        }
        Ok(None) => {
            send_response(
                stream,
                401,
                "application/json",
                r#"{"error":"invalid password or login not configured"}"#,
            )
        }
        Err(e) => send_response(
            stream,
            500,
            "application/json",
            &format!(r#"{{"error":"{}"}}"#, e),
        ),
    }
}

/// Server-Sent Events: stream de eventos do Blackboard.
fn api_events(stream: &mut TcpStream, bb: Blackboard) -> Result<()> {
    let headers = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Connection: keep-alive\r\n\
        Access-Control-Allow-Origin: *\r\n\r\n";
    stream.write_all(headers.as_bytes())?;
    stream.flush()?;

    // Streama eventos por 60 segundos ou até conexão fechar
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        // Coleta eventos de todas as categorias (simulado: pub/sub do blackboard)
        if let Some((cat, key, val)) = bb.next_event_any() {
            let payload = serde_json::json!({
                "category": cat,
                "key": key,
                "value": val,
                "time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            });
            let event = format!("data: {}\n\n", payload);
            if stream.write_all(event.as_bytes()).is_err() {
                break;
            }
            stream.flush()?;
        } else {
            thread::sleep(Duration::from_millis(200));
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

// ── Utilitários HTTP ──────────────────────────────────────────────────────────

fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<()> {
    let status_text = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n\
         {}",
        status,
        status_text,
        content_type,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

// Extensão do Blackboard para consumir qualquer evento (helper temporário)
trait BlackboardEventAny {
    fn next_event_any(&self) -> Option<(String, String, Value)>;
}

impl BlackboardEventAny for Blackboard {
    fn next_event_any(&self) -> Option<(String, String, Value)> {
        // Tenta categorias comuns
        for cat in &["metrics", "interrupt", "permission_request", "event"] {
            if let Some(ev) = self.next_event(cat) {
                return Some((cat.to_string(), ev.id, ev.data));
            }
        }
        None
    }
}

// ── HITL Handlers (PVC-Q1.2) ──────────────────────────────────────────────────

fn api_pending_approvals(
    stream: &mut TcpStream,
    bb: Blackboard,
    auth: &AuthMiddleware,
    auth_ctx: Option<&AuthContext>,
) -> Result<()> {
    // RBAC: requer HitlReadAll ou AuditRead ou Admin
    let authorized = match auth_ctx {
        Some(ctx) => {
            auth.has_permission(ctx, arreio_security::Permission::HitlReadAll)
                || auth.has_permission(ctx, arreio_security::Permission::AuditRead)
                || ctx.role == "admin"
        }
        None => false,
    };
    if !authorized {
        return send_response(
            stream,
            403,
            "application/json",
            r#"{"error":"forbidden","message":"Requer permissão HitlReadAll ou AuditRead"}"#,
        );
    }

    // Busca tuplas hitl_decision::* no Blackboard
    let pending: Vec<Value> = bb
        .search_tuples("hitl_decision", "")
        .into_iter()
        .filter_map(|(key, value)| {
            // Verifica se ainda está pendente (não tem decisão final gravada)
            // Simplificação: lista todas as tuplas hitl_decision
            let obj = value.as_object()?;
            Some(serde_json::json!({
                "task_id": key,
                "status": obj.get("decision").and_then(|v| v.as_str()).unwrap_or("Pending"),
                "requested_at": obj.get("timestamp"),
                "policy_name": obj.get("policy_name"),
            }))
        })
        .collect();

    let json = serde_json::json!({ "approvals": pending });
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_approve(
    stream: &mut TcpStream,
    bb: Blackboard,
    auth: &AuthMiddleware,
    auth_ctx: Option<&AuthContext>,
    task_id: &str,
    body: &str,
) -> Result<()> {
    let authorized = match auth_ctx {
        Some(ctx) => {
            auth.has_permission(ctx, arreio_security::Permission::HitlApprove) || ctx.role == "admin"
        }
        None => false,
    };
    if !authorized {
        return send_response(
            stream,
            403,
            "application/json",
            r#"{"error":"forbidden","message":"Requer permissão HitlApprove"}"#,
        );
    }

    let justification: Option<String> = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("justification").and_then(|j| j.as_str().map(|s| s.to_string())));

    let approver = auth_ctx.map(|c| c.client_id.clone()).unwrap_or_else(|| "unknown".into());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let decision = arreio_kernel::HumanDecision {
        task_id: task_id.into(),
        decision: arreio_kernel::ApprovalDecision::Approved,
        approver_identity: approver.clone(),
        approver_roles: vec![auth_ctx.map(|c| c.role.clone()).unwrap_or_default()],
        context_hash: String::new(), // Simplificado — em produção, computar hash real
        timestamp,
        justification,
        policy_name: None,
        escalation_level: 0,
    };

    // Grava no Blackboard
    let payload = serde_json::to_value(&decision)?;
    bb.put_tuple("hitl_decision", task_id, payload)?;

    let json = serde_json::json!({
        "status": "approved",
        "task_id": task_id,
        "approver": approver,
        "timestamp": timestamp,
    });
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_reject(
    stream: &mut TcpStream,
    bb: Blackboard,
    auth: &AuthMiddleware,
    auth_ctx: Option<&AuthContext>,
    task_id: &str,
    body: &str,
) -> Result<()> {
    let authorized = match auth_ctx {
        Some(ctx) => {
            auth.has_permission(ctx, arreio_security::Permission::HitlReject) || ctx.role == "admin"
        }
        None => false,
    };
    if !authorized {
        return send_response(
            stream,
            403,
            "application/json",
            r#"{"error":"forbidden","message":"Requer permissão HitlReject"}"#,
        );
    }

    let justification: Option<String> = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("justification").and_then(|j| j.as_str().map(|s| s.to_string())));

    let approver = auth_ctx.map(|c| c.client_id.clone()).unwrap_or_else(|| "unknown".into());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let decision = arreio_kernel::HumanDecision {
        task_id: task_id.into(),
        decision: arreio_kernel::ApprovalDecision::Rejected,
        approver_identity: approver.clone(),
        approver_roles: vec![auth_ctx.map(|c| c.role.clone()).unwrap_or_default()],
        context_hash: String::new(),
        timestamp,
        justification: justification.clone(),
        policy_name: None,
        escalation_level: 0,
    };

    let payload = serde_json::to_value(&decision)?;
    bb.put_tuple("hitl_decision", task_id, payload)?;

    let json = serde_json::json!({
        "status": "rejected",
        "task_id": task_id,
        "approver": approver,
        "timestamp": timestamp,
        "reason": justification,
    });
    send_response(stream, 200, "application/json", &json.to_string())
}

fn api_approval_status(
    stream: &mut TcpStream,
    bb: Blackboard,
    task_id: &str,
) -> Result<()> {
    let status = match bb.get_tuple("hitl_decision", task_id) {
        Some(value) => {
            let decision: Option<String> = value
                .get("decision")
                .and_then(|v| v.as_str().map(|s| s.to_string()));
            serde_json::json!({
                "task_id": task_id,
                "status": decision.unwrap_or_else(|| "Pending".into()),
                "details": value,
            })
        }
        None => {
            serde_json::json!({
                "task_id": task_id,
                "status": "NotFound",
            })
        }
    };
    send_response(stream, 200, "application/json", &status.to_string())
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openai_system_and_user() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "Você é um assistente."},
                {"role": "user", "content": "Olá"},
                {"role": "user", "content": "Mundo"}
            ]
        });
        let (system, user) = parse_openai_messages(&payload);
        assert_eq!(system, "Você é um assistente.");
        assert_eq!(user, "Olá\n\nMundo");
    }

    #[test]
    fn parse_openai_sem_system() {
        let payload = serde_json::json!({
            "messages": [
                {"role": "user", "content": "Só user"}
            ]
        });
        let (system, user) = parse_openai_messages(&payload);
        assert!(system.is_empty());
        assert_eq!(user, "Só user");
    }

    #[test]
    fn parse_openai_ignora_assistant() {
        let payload = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": "Resposta anterior"},
                {"role": "user", "content": "Nova pergunta"}
            ]
        });
        let (system, user) = parse_openai_messages(&payload);
        assert!(system.is_empty());
        assert_eq!(user, "Nova pergunta");
    }

    // Regressão do bug do /health: api_health passava `None` como caminho do
    // Blackboard, caindo sempre no ramo "caminho não verificado" → status Degraded
    // enganoso. O fix passa `Some(bb.store_path())`. Double-entry: provamos que o
    // caminho real dá Healthy E que o `None` antigo dava Degraded (isola a causa).
    #[test]
    fn health_blackboard_com_caminho_real_fica_healthy_nao_degraded() {
        use tempfile::NamedTempFile;
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&path).unwrap();
        bb.persist_now().unwrap();
        assert!(bb.store_path().exists(), "db do blackboard deveria existir em disco");

        let collector = arreio_telemetry::MetricsCollector::new(bb.clone());

        // Caminho do FIX: Some(store_path) com db existente → Healthy.
        let fixed = arreio_telemetry::HealthProbe::check_all(Some(bb.store_path()), Some(&collector));
        let fixed_bb = fixed.iter().find(|r| r.name == "blackboard").expect("sonda blackboard");
        assert_eq!(
            fixed_bb.status,
            arreio_telemetry::HealthStatus::Healthy,
            "com caminho real e db existente deve ser Healthy; msg={}",
            fixed_bb.message
        );

        // Caminho do BUG (contraste): None → Degraded. Garante que o fix é a causa.
        let buggy = arreio_telemetry::HealthProbe::check_all(None, Some(&collector));
        let buggy_bb = buggy.iter().find(|r| r.name == "blackboard").unwrap();
        assert_eq!(buggy_bb.status, arreio_telemetry::HealthStatus::Degraded);
    }
}
