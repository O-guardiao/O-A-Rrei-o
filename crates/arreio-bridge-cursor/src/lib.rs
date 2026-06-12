//! Arreio-Bridge-Cursor — Adapter para Cursor IDE (Anysphere).
//!
//! Expõe O Arreio como MCP Server SSE que Cursor IDE consome.
//! Cliente da Cloud Agents API v1 do Cursor para delegação de tarefas.
//! Sandbox: execuções Cursor redirecionadas para Hypervisor local.

use anyhow::{bail, Context, Result};
use arreio_mcp::protocol::*;
use arreio_mcp::{McpInitializeResult, McpTool, McpToolCall, McpToolResult};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════════════════════
// CursorMcpServer
// ═══════════════════════════════════════════════════════════════════════════════

/// MCP Server SSE que o Cursor IDE consome.
///
/// Encapsula um `ArreioMcpServer` e expõe endpoints HTTP síncronos:
/// - `GET /sse` → stream de eventos Server-Sent Events
/// - `POST /message` → recebe mensagens JSON-RPC do Cursor
pub struct CursorMcpServer {
    pub arreio_mcp: arreio_mcp_server::ArreioMcpServer,
    pub addr: String,
}

impl CursorMcpServer {
    /// Constrói o servidor com a instância compartilhada do MCP.
    pub fn new(arreio_mcp: arreio_mcp_server::ArreioMcpServer, addr: String) -> Self {
        Self { arreio_mcp, addr }
    }

    /// Serve MCP sobre SSE (Server-Sent Events).
    ///
    /// Endpoint: `GET /sse` → inicia stream SSE e envia URL do endpoint POST.
    /// Endpoint: `POST /message?session_id=<id>` → recebe JSON-RPC; resposta
    /// é enfileirada e entregue via SSE.
    pub fn serve_sse(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.addr)
            .with_context(|| format!("falha ao bind SSE em {}", self.addr))?;
        println!("[cursor-mcp] SSE ouvindo em http://{}", self.addr);

        let bb = self.arreio_mcp.blackboard.clone();
        let timeout = self.arreio_mcp.hypervisor.timeout();
        let fsm_bb = self.arreio_mcp.blackboard.clone();
        let sessions: Arc<Mutex<HashMap<String, Vec<String>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let bb = bb.clone();
                    let fsm_bb = fsm_bb.clone();
                    let sessions = Arc::clone(&sessions);
                    thread::spawn(move || {
                        let _ = handle_cursor_connection(stream, bb, timeout, fsm_bb, sessions);
                    });
                }
                Err(e) => eprintln!("[cursor-mcp] erro de conexão: {}", e),
            }
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// HTTP / SSE handlers
// ═══════════════════════════════════════════════════════════════════════════════

fn handle_cursor_connection(
    mut stream: TcpStream,
    bb: arreio_kernel::Blackboard,
    timeout: u64,
    fsm_bb: arreio_kernel::Blackboard,
    sessions: Arc<Mutex<HashMap<String, Vec<String>>>>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(&stream);

    // Lê a primeira linha: METHOD PATH HTTP/1.1
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let parts: Vec<&str> = first_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        return send_http_response(&mut stream, 400, "text/plain", "Bad Request");
    }
    let method = parts[0];
    let path = parts[1];

    // Lê headers até linha em branco
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        headers.push(line);
    }

    // Lê body se Content-Length presente
    let mut body = String::new();
    for h in &headers {
        if h.to_lowercase().starts_with("content-length:") {
            if let Some(len_str) = h.split(':').nth(1) {
                if let Ok(len) = len_str.trim().parse::<usize>() {
                    let mut buf = vec![0u8; len];
                    reader.read_exact(&mut buf)?;
                    body = String::from_utf8_lossy(&buf).into_owned();
                }
            }
        }
    }

    match (method, path) {
        ("GET", "/sse") => handle_sse_stream(stream, sessions),
        ("POST", path) if path.starts_with("/message") => {
            handle_message_post(stream, path, &body, bb, timeout, fsm_bb, sessions)
        }
        _ => send_http_response(
            &mut stream,
            404,
            "application/json",
            r#"{"error":"not found"}"#,
        ),
    }
}

/// Manipula a conexão SSE: envia endpoint e fica em loop de entrega.
fn handle_sse_stream(
    mut stream: TcpStream,
    sessions: Arc<Mutex<HashMap<String, Vec<String>>>>,
) -> Result<()> {
    let session_id = uuid::Uuid::new_v4().to_string();
    {
        let mut map = sessions.lock().unwrap();
        map.insert(session_id.clone(), Vec::new());
    }

    let headers = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Connection: keep-alive\r\n\
        Access-Control-Allow-Origin: *\r\n\r\n";
    stream.write_all(headers.as_bytes())?;
    stream.flush()?;

    // Envia o endpoint de POST para esta sessão
    let endpoint_event = format!(
        "event: endpoint\ndata: /message?session_id={}\n\n",
        session_id
    );
    stream.write_all(endpoint_event.as_bytes())?;
    stream.flush()?;

    let start = std::time::Instant::now();
    let mut last_keepalive = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        let msgs: Vec<String> = {
            let mut map = sessions.lock().unwrap();
            map.get_mut(&session_id)
                .map(|v| v.drain(..).collect())
                .unwrap_or_default()
        };

        for msg in msgs {
            let event = format!("event: message\ndata: {}\n\n", msg);
            if stream.write_all(event.as_bytes()).is_err() {
                break;
            }
            if stream.flush().is_err() {
                break;
            }
        }

        if last_keepalive.elapsed() >= Duration::from_secs(1) {
            if stream.write_all(b":keepalive\n\n").is_err() {
                break;
            }
            if stream.flush().is_err() {
                break;
            }
            last_keepalive = std::time::Instant::now();
        }

        thread::sleep(Duration::from_millis(200));
    }

    let _ = stream.shutdown(Shutdown::Both);
    let mut map = sessions.lock().unwrap();
    map.remove(&session_id);
    Ok(())
}

/// Recebe mensagem JSON-RPC via POST e a processa.
fn handle_message_post(
    mut stream: TcpStream,
    path: &str,
    body: &str,
    bb: arreio_kernel::Blackboard,
    timeout: u64,
    fsm_bb: arreio_kernel::Blackboard,
    sessions: Arc<Mutex<HashMap<String, Vec<String>>>>,
) -> Result<()> {
    // Extrai session_id, se presente
    let session_id = path.split("session_id=").nth(1).unwrap_or("").to_string();

    let hv = arreio_hypervisor::Hypervisor::new(timeout);
    let fsm = arreio_fsm::Fsm::new(fsm_bb);
    let srv = arreio_mcp_server::ArreioMcpServer::new(bb, hv, fsm);

    let req: JsonRpcMessage<Value> = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return send_http_response(
                &mut stream,
                400,
                "application/json",
                &json_error_str(&e.to_string()),
            );
        }
    };

    let resp = handle_jsonrpc_local(&srv, req);

    // Se houver sessão, enfileira a resposta para entrega SSE e retorna 202
    if !session_id.is_empty() {
        let json = serde_json::to_string(&resp)?;
        let mut map = sessions.lock().unwrap();
        if let Some(buf) = map.get_mut(&session_id) {
            buf.push(json);
        }
        send_http_response(
            &mut stream,
            202,
            "application/json",
            r#"{"status":"accepted"}"#,
        )
    } else {
        let json = serde_json::to_string(&resp)?;
        send_http_response(&mut stream, 200, "application/json", &json)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// JSON-RPC local (réplica da lógica do ArreioMcpServer para uso no bridge)
// ═══════════════════════════════════════════════════════════════════════════════

fn handle_jsonrpc_local(
    srv: &arreio_mcp_server::ArreioMcpServer,
    req: JsonRpcMessage<Value>,
) -> JsonRpcMessage<Value> {
    let method = req.method.as_deref().unwrap_or("");
    let result = match method {
        "initialize" => match handle_initialize_local(srv) {
            Ok(v) => v,
            Err(e) => return jsonrpc_error(req.id, -32603, &e.to_string()),
        },
        "tools/list" => serde_json::to_value(list_tools_local(srv)).unwrap_or(Value::Null),
        "tools/call" => {
            let params = req.params.unwrap_or(Value::Null);
            let call: McpToolCall = match serde_json::from_value(params) {
                Ok(c) => c,
                Err(e) => return jsonrpc_error(req.id, -32602, &e.to_string()),
            };
            match call_tool_local(srv, call) {
                Ok(r) => serde_json::to_value(r).unwrap_or(Value::Null),
                Err(e) => return jsonrpc_error(req.id, -32603, &e.to_string()),
            }
        }
        "resources/read" => {
            let uri = req
                .params
                .as_ref()
                .and_then(|p| p.get("uri"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match read_resource_local(srv, uri) {
                Ok(v) => Value::String(v),
                Err(e) => return jsonrpc_error(req.id, -32603, &e.to_string()),
            }
        }
        "prompts/get" => {
            let name = req
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match get_prompt_local(name) {
                Ok(v) => Value::String(v),
                Err(e) => return jsonrpc_error(req.id, -32603, &e.to_string()),
            }
        }
        _ => return jsonrpc_error(req.id, -32601, "Method not found"),
    };

    JsonRpcMessage {
        jsonrpc: "2.0".into(),
        id: req.id,
        method: None,
        params: None,
        result: Some(result),
        error: None,
    }
}

fn handle_initialize_local(_srv: &arreio_mcp_server::ArreioMcpServer) -> Result<Value> {
    let mut caps = HashMap::new();
    caps.insert("tools".into(), serde_json::json!({"listChanged": false}));
    caps.insert("resources".into(), serde_json::json!({"subscribe": false}));
    caps.insert("prompts".into(), serde_json::json!({"listChanged": false}));
    let result = McpInitializeResult {
        protocol_version: "2024-11-05".into(),
        capabilities: caps,
        server_info: arreio_mcp::protocol::ServerInfo {
            name: "arreio-cursor".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    };
    Ok(serde_json::to_value(result)?)
}

fn list_tools_local(srv: &arreio_mcp_server::ArreioMcpServer) -> Vec<McpTool> {
    srv.capabilities
        .iter()
        .filter_map(|c| match c {
            arreio_mcp_server::McpCapability::Tool { name, description } => Some(McpTool {
                name: name.clone(),
                description: description.clone(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            }),
            _ => None,
        })
        .collect()
}

fn call_tool_local(
    srv: &arreio_mcp_server::ArreioMcpServer,
    call: McpToolCall,
) -> Result<McpToolResult> {
    match call.name.as_str() {
        "create_task" => {
            let mut dag = arreio_dag::Dag::load(srv.blackboard.clone())?;
            let spec = call
                .arguments
                .get("spec")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text = arreio_mcp_server::create_task_tool(&mut dag, spec)?;
            Ok(mcp_text_result(&text))
        }
        "checkpoint_rollback" => {
            let mut dag = arreio_dag::Dag::load(srv.blackboard.clone())?;
            let id = call
                .arguments
                .get("checkpoint_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text = arreio_mcp_server::checkpoint_rollback_tool(&mut dag, id)?;
            Ok(mcp_text_result(&text))
        }
        "safe_execute" => {
            let cmd = call
                .arguments
                .get("cmd")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text = arreio_mcp_server::safe_execute_tool(&srv.hypervisor, cmd)?;
            Ok(mcp_text_result(&text))
        }
        "blackboard_read" => {
            let cat = call
                .arguments
                .get("cat")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let key = call
                .arguments
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match arreio_mcp_server::blackboard_read_tool(&srv.blackboard, cat, key)? {
                Some(text) => Ok(mcp_text_result(&text)),
                None => Ok(McpToolResult {
                    content: vec![],
                    is_error: false,
                }),
            }
        }
        "blackboard_write" => {
            let cat = call
                .arguments
                .get("cat")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let key = call
                .arguments
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let value = call
                .arguments
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut bb = srv.blackboard.clone();
            arreio_mcp_server::blackboard_write_tool(&mut bb, cat, key, value)?;
            Ok(mcp_text_result("ok"))
        }
        "dag_status" => {
            let dag = arreio_dag::Dag::load(srv.blackboard.clone())?;
            let text = arreio_mcp_server::dag_status_tool(&dag)?;
            Ok(mcp_text_result(&text))
        }
        _ => bail!("tool desconhecida: {}", call.name),
    }
}

fn read_resource_local(srv: &arreio_mcp_server::ArreioMcpServer, uri: &str) -> Result<String> {
    if let Some(rest) = uri.strip_prefix("blackboard://") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("URI de blackboard inválida: {}", uri);
        }
        match arreio_mcp_server::resolve_blackboard_resource(&srv.blackboard, parts[0], parts[1])? {
            Some(v) => Ok(v),
            None => bail!("tupla não encontrada: {}::{}", parts[0], parts[1]),
        }
    } else if let Some(task_id) = uri.strip_prefix("dag://") {
        let dag = arreio_dag::Dag::load(srv.blackboard.clone())?;
        match arreio_mcp_server::resolve_dag_resource(&dag, task_id)? {
            Some(v) => Ok(v),
            None => bail!("tarefa não encontrada: {}", task_id),
        }
    } else if let Some(actor_id) = uri.strip_prefix("fsm://") {
        Ok(arreio_mcp_server::resolve_fsm_resource(&srv.fsm, actor_id)?)
    } else {
        bail!("esquema de URI não suportado: {}", uri)
    }
}

fn get_prompt_local(name: &str) -> Result<String> {
    match name {
        "planning" => Ok(arreio_mcp_server::planning_prompt()),
        "review" => Ok(arreio_mcp_server::review_prompt()),
        "security_audit" => Ok(arreio_mcp_server::security_audit_prompt()),
        _ => bail!("prompt desconhecido: {}", name),
    }
}

fn mcp_text_result(text: &str) -> McpToolResult {
    McpToolResult {
        content: vec![arreio_mcp::protocol::ToolContent {
            content_type: "text".into(),
            text: text.into(),
        }],
        is_error: false,
    }
}

fn jsonrpc_error(id: Option<u64>, code: i32, message: &str) -> JsonRpcMessage<Value> {
    JsonRpcMessage {
        jsonrpc: "2.0".into(),
        id,
        method: None,
        params: None,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
        }),
    }
}

fn json_error_str(msg: &str) -> String {
    serde_json::json!({"error": msg}).to_string()
}

// ═══════════════════════════════════════════════════════════════════════════════
// Utilitários HTTP
// ═══════════════════════════════════════════════════════════════════════════════

fn send_http_response(
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
        500 => "Internal Server Error",
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
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════════
// CursorCloudClient
// ═══════════════════════════════════════════════════════════════════════════════

/// Cliente para Cursor Cloud Agents API.
/// Por padrão retorna stub, mas se `CURSOR_CLOUD_ENDPOINT` estiver definido
/// no ambiente, tenta POST real via curl.
pub struct CursorCloudClient {
    pub api_key: Option<String>,
    pub endpoint: String,
}

impl CursorCloudClient {
    /// Cria o cliente. Endpoint padrão lê `CURSOR_CLOUD_ENDPOINT` do env;
    /// fallback para `https://api.cursor.com/v1`.
    pub fn new(api_key: Option<String>) -> Self {
        let endpoint = std::env::var("CURSOR_CLOUD_ENDPOINT")
            .unwrap_or_else(|_| "https://api.cursor.com/v1".to_string());
        Self { api_key, endpoint }
    }

    /// Delega tarefa para Cursor Cloud.
    ///
    /// Se `CURSOR_CLOUD_ENDPOINT` não foi definido pelo usuário, retorna stub
    /// explícito. Caso contrário, tenta POST via curl com o task_spec.
    pub fn delegate(&self, task_spec: &str) -> anyhow::Result<String> {
        let env_endpoint = std::env::var("CURSOR_CLOUD_ENDPOINT").unwrap_or_default();
        if env_endpoint.is_empty() {
            eprintln!(
                "[cursor] Cursor Cloud API não configurada. \
                 Defina CURSOR_CLOUD_ENDPOINT para habilitar. task_spec='{}'",
                task_spec
            );
            return Ok(format!(
                r#"{{"status":"unconfigured","reason":"CURSOR_CLOUD_ENDPOINT não definido","task_spec":"{}"}}"#,
                task_spec
            ));
        }

        let body = serde_json::json!({
            "task_spec": task_spec,
            "api_key": self.api_key.as_deref().unwrap_or(""),
        });
        let mut cmd = std::process::Command::new("curl");
        cmd.args([
            "-sS", "-X", "POST",
            "-H", "Content-Type: application/json",
            "-d", &body.to_string(),
            &self.endpoint,
        ]);
        let output = cmd.output()
            .with_context(|| "curl não disponível no sistema")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Cursor Cloud POST falhou: {}", stderr);
        }
        let resp = String::from_utf8_lossy(&output.stdout);
        Ok(resp.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// CursorSandbox
// ═══════════════════════════════════════════════════════════════════════════════

/// Redireciona execuções Cursor para Hypervisor local.
///
/// Executa `cmd` no sandbox do `Hypervisor` e retorna resultado como JSON.
pub fn sandbox_execute(hypervisor: &arreio_hypervisor::Hypervisor, cmd: &str) -> Result<String> {
    let result = hypervisor
        .run(cmd, None)
        .with_context(|| format!("sandbox_execute falhou: {}", cmd))?;
    Ok(serde_json::json!({
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "elapsed_ms": result.elapsed.as_millis(),
    })
    .to_string())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_server() -> arreio_mcp_server::ArreioMcpServer {
        let bb = temp_bb();
        let hv = arreio_hypervisor::Hypervisor::new(10);
        let fsm = arreio_fsm::Fsm::new(bb.clone());
        arreio_mcp_server::ArreioMcpServer::new(bb, hv, fsm)
    }

    // 1
    #[test]
    fn cursor_mcp_server_instancia() {
        let srv = make_server();
        let cursor = CursorMcpServer::new(srv, "127.0.0.1:9999".to_string());
        assert_eq!(cursor.addr, "127.0.0.1:9999");
    }

    // 2
    #[test]
    fn cursor_cloud_client_com_api_key() {
        let c = CursorCloudClient::new(Some("sk-cursor".to_string()));
        assert_eq!(c.api_key, Some("sk-cursor".to_string()));
        assert_eq!(c.endpoint, "https://api.cursor.com/v1");
    }

    // 3
    #[test]
    fn cursor_cloud_client_sem_api_key() {
        let c = CursorCloudClient::new(None);
        assert!(c.api_key.is_none());
    }

    // 4
    #[test]
    fn cursor_cloud_delegate_retorna_unconfigured() {
        let c = CursorCloudClient::new(None);
        let r = c.delegate("refatorar modulo").unwrap();
        assert!(r.contains("unconfigured"));
        assert!(r.contains("CURSOR_CLOUD_ENDPOINT"));
        assert!(r.contains("refatorar modulo"));
    }

    // 5
    #[test]
    fn sandbox_execute_comando_seguro() {
        let hv = arreio_hypervisor::Hypervisor::new(10);
        #[cfg(target_os = "windows")]
        let r = sandbox_execute(&hv, "echo hello-cursor").unwrap();
        #[cfg(not(target_os = "windows"))]
        let r = sandbox_execute(&hv, "echo hello-cursor").unwrap();
        assert!(r.contains("hello-cursor"));
        assert!(r.contains("exit_code"));
    }

    // 6
    #[test]
    fn sandbox_execute_comando_bloqueado() {
        let hv = arreio_hypervisor::Hypervisor::new(10);
        // Error withholding: bloqueios retornam Ok com exit_code=-3 e permission_denied
        let result = sandbox_execute(&hv, "rm -rf /tmp/test").unwrap();
        assert!(result.contains("exit_code"));
        assert!(result.contains("-3"));
        assert!(result.contains("PERMISSION DENIED"));
    }

    // 7
    #[test]
    fn sse_endpoint_retorna_event_stream() {
        let srv = make_server();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cursor = CursorMcpServer::new(srv, format!("127.0.0.1:{}", port));
        thread::spawn(move || {
            let _ = cursor.serve_sse();
        });
        thread::sleep(Duration::from_millis(150));

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .write_all(b"GET /sse HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();

        thread::sleep(Duration::from_millis(100));

        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"), "resp: {}", resp);
        assert!(resp.contains("text/event-stream"), "resp: {}", resp);
        assert!(resp.contains("event: endpoint"), "resp: {}", resp);
    }

    // 8
    #[test]
    fn post_message_initialize() {
        let srv = make_server();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cursor = CursorMcpServer::new(srv, format!("127.0.0.1:{}", port));
        thread::spawn(move || {
            let _ = cursor.serve_sse();
        });
        thread::sleep(Duration::from_millis(150));

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req = format!(
            "POST /message HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n\
             {}",
            body.len(),
            body
        );
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"), "resp: {}", resp);
        assert!(resp.contains("arreio-cursor"), "resp: {}", resp);
    }

    // 9
    #[test]
    fn post_message_tools_list() {
        let srv = make_server();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cursor = CursorMcpServer::new(srv, format!("127.0.0.1:{}", port));
        thread::spawn(move || {
            let _ = cursor.serve_sse();
        });
        thread::sleep(Duration::from_millis(150));

        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let req = format!(
            "POST /message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"), "resp: {}", resp);
        assert!(resp.contains("blackboard_read"), "resp: {}", resp);
        assert!(resp.contains("safe_execute"), "resp: {}", resp);
    }

    // 10
    #[test]
    fn post_message_tool_call_blackboard() {
        let srv = make_server();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cursor = CursorMcpServer::new(srv, format!("127.0.0.1:{}", port));
        thread::spawn(move || {
            let _ = cursor.serve_sse();
        });
        thread::sleep(Duration::from_millis(150));

        // Escreve no blackboard
        let write_body = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"blackboard_write","arguments":{"cat":"cursor","key":"k1","value":"\"valor-teste\""}}}"#;
        let req1 = format!(
            "POST /message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            write_body.len(), write_body
        );
        let mut stream1 = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream1.write_all(req1.as_bytes()).unwrap();
        stream1.flush().unwrap();
        let mut buf1 = [0u8; 2048];
        let n1 = stream1.read(&mut buf1).unwrap();
        let resp1 = String::from_utf8_lossy(&buf1[..n1]);
        assert!(resp1.contains("200 OK"), "resp1: {}", resp1);
        assert!(resp1.contains("ok"), "resp1: {}", resp1);

        // Lê do blackboard
        let read_body = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"blackboard_read","arguments":{"cat":"cursor","key":"k1"}}}"#;
        let req2 = format!(
            "POST /message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            read_body.len(), read_body
        );
        let mut stream2 = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream2.write_all(req2.as_bytes()).unwrap();
        stream2.flush().unwrap();
        let mut buf2 = [0u8; 2048];
        let n2 = stream2.read(&mut buf2).unwrap();
        let resp2 = String::from_utf8_lossy(&buf2[..n2]);
        assert!(resp2.contains("200 OK"), "resp2: {}", resp2);
        assert!(resp2.contains("valor-teste"), "resp2: {}", resp2);
    }

    // 11
    #[test]
    fn post_message_metodo_desconhecido() {
        let srv = make_server();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cursor = CursorMcpServer::new(srv, format!("127.0.0.1:{}", port));
        thread::spawn(move || {
            let _ = cursor.serve_sse();
        });
        thread::sleep(Duration::from_millis(150));

        let body = r#"{"jsonrpc":"2.0","id":5,"method":"foo/bar"}"#;
        let req = format!(
            "POST /message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("200 OK"), "resp: {}", resp);
        assert!(resp.contains("-32601"), "resp: {}", resp);
        assert!(resp.contains("Method not found"), "resp: {}", resp);
    }

    // 12
    #[test]
    fn post_message_com_session_id_retorna_202() {
        let srv = make_server();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let cursor = CursorMcpServer::new(srv, format!("127.0.0.1:{}", port));
        thread::spawn(move || {
            let _ = cursor.serve_sse();
        });
        thread::sleep(Duration::from_millis(150));

        let body = r#"{"jsonrpc":"2.0","id":6,"method":"initialize","params":{}}"#;
        let req = format!(
            "POST /message?session_id=abc123 HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n\
             {}",
            body.len(),
            body
        );
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.contains("202 Accepted"), "resp: {}", resp);
        assert!(resp.contains("accepted"), "resp: {}", resp);
    }
}
