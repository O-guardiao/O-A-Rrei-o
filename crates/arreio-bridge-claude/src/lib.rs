//! Arreio-Bridge-Claude — Adapter para Claude Code (Anthropic).
//!
//! Expõe O Arreio como MCP Server stdio que Claude Code consome via `/mcp`.
//! Fallback: wrapper `claude -p <prompt>` que orquestra via Hypervisor.
//! Traduz `CLAUDE.md` hierarchy para tuplas Blackboard.

use anyhow::{bail, Context, Result};
use arreio_mcp::protocol::{JsonRpcError, JsonRpcMessage, ServerInfo};
use arreio_mcp::{McpInitializeResult, McpTool, McpToolCall, McpToolResult};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};

// ═══════════════════════════════════════════════════════════════════════════════
// ClaudeMcpServer
// ═══════════════════════════════════════════════════════════════════════════════

/// MCP Server stdio consumido pelo Claude Code via `/mcp`.
/// Delega chamadas JSON-RPC para o `ArreioMcpServer` do Arreio.
pub struct ClaudeMcpServer {
    arreio_mcp: arreio_mcp_server::ArreioMcpServer,
}

impl ClaudeMcpServer {
    pub fn new(arreio_mcp: arreio_mcp_server::ArreioMcpServer) -> Self {
        Self { arreio_mcp }
    }

    /// Serve MCP sobre stdio (JSON-RPC 2.0).
    /// Lê linhas de stdin, parseia JSON-RPC, delega para arreio_mcp_server, escreve resposta em stdout.
    pub fn serve_stdio(&self) -> Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        self.serve_stdio_with(stdin.lock(), stdout.lock())
    }

    /// Versão testável de `serve_stdio` aceitando leitor e escritor genéricos.
    fn serve_stdio_with<R: BufRead, W: Write>(&self, mut reader: R, mut writer: W) -> Result<()> {
        loop {
            let mut content_length: Option<usize> = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 {
                    return Ok(()); // EOF
                }
                if line.trim().is_empty() {
                    break;
                }
                if line.to_lowercase().starts_with("content-length:") {
                    content_length = line.split(':').nth(1).and_then(|s| s.trim().parse().ok());
                }
            }

            let len = match content_length {
                Some(n) => n,
                None => continue,
            };

            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf)?;

            let req: JsonRpcMessage<Value> = serde_json::from_slice(&buf)?;
            let resp = self.handle_jsonrpc(req)?;

            let json = serde_json::to_string(&resp)?;
            let header = format!("Content-Length: {}\r\n\r\n", json.len());
            writer.write_all(header.as_bytes())?;
            writer.write_all(json.as_bytes())?;
            writer.flush()?;
        }
    }

    fn handle_jsonrpc(&self, req: JsonRpcMessage<Value>) -> Result<JsonRpcMessage<Value>> {
        let method = req.method.as_deref().unwrap_or("");
        let result = match method {
            "initialize" => self.handle_initialize()?,
            "tools/list" => serde_json::to_value(self.list_tools())?,
            "tools/call" => {
                let call: McpToolCall = serde_json::from_value(req.params.unwrap_or(Value::Null))?;
                serde_json::to_value(self.call_tool(call)?)?
            }
            "resources/read" => {
                let uri = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("uri"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                serde_json::to_value(self.read_resource(uri)?)?
            }
            "prompts/get" => {
                let name = req
                    .params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                serde_json::to_value(self.get_prompt(name)?)?
            }
            _ => {
                return Ok(JsonRpcMessage {
                    jsonrpc: "2.0".into(),
                    id: req.id,
                    method: None,
                    params: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32601,
                        message: "Method not found".into(),
                    }),
                });
            }
        };

        Ok(JsonRpcMessage {
            jsonrpc: "2.0".into(),
            id: req.id,
            method: None,
            params: None,
            result: Some(result),
            error: None,
        })
    }

    fn handle_initialize(&self) -> Result<Value> {
        let mut caps = HashMap::new();
        caps.insert("tools".into(), serde_json::json!({"listChanged": false}));
        caps.insert("resources".into(), serde_json::json!({"subscribe": false}));
        caps.insert("prompts".into(), serde_json::json!({"listChanged": false}));
        let result = McpInitializeResult {
            protocol_version: "2024-11-05".into(),
            capabilities: caps,
            server_info: ServerInfo {
                name: "arreio-mcp".into(),
                version: "0.2.0".into(),
            },
        };
        Ok(serde_json::to_value(result)?)
    }

    fn list_tools(&self) -> Vec<McpTool> {
        self.arreio_mcp
            .capabilities
            .iter()
            .filter_map(|c| match c {
                arreio_mcp_server::McpCapability::Tool { name, description } => Some(McpTool {
                    name: name.clone(),
                    description: description.clone(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {},
                    }),
                }),
                _ => None,
            })
            .collect()
    }

    fn call_tool(&self, call: McpToolCall) -> Result<McpToolResult> {
        match call.name.as_str() {
            "create_task" => {
                let mut dag = arreio_dag::Dag::load(self.arreio_mcp.blackboard.clone())?;
                let spec = call
                    .arguments
                    .get("spec")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let text = arreio_mcp_server::create_task_tool(&mut dag, spec)?;
                Ok(mcp_text_result(&text))
            }
            "checkpoint_rollback" => {
                let mut dag = arreio_dag::Dag::load(self.arreio_mcp.blackboard.clone())?;
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
                let text = arreio_mcp_server::safe_execute_tool(&self.arreio_mcp.hypervisor, cmd)?;
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
                match arreio_mcp_server::blackboard_read_tool(&self.arreio_mcp.blackboard, cat, key)? {
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
                let mut bb = self.arreio_mcp.blackboard.clone();
                arreio_mcp_server::blackboard_write_tool(&mut bb, cat, key, value)?;
                Ok(mcp_text_result("ok"))
            }
            "dag_status" => {
                let dag = arreio_dag::Dag::load(self.arreio_mcp.blackboard.clone())?;
                let text = arreio_mcp_server::dag_status_tool(&dag)?;
                Ok(mcp_text_result(&text))
            }
            _ => bail!("tool desconhecida: {}", call.name),
        }
    }

    fn read_resource(&self, uri: &str) -> Result<String> {
        if let Some(rest) = uri.strip_prefix("blackboard://") {
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() != 2 {
                bail!("URI de blackboard inválida: {}", uri);
            }
            match arreio_mcp_server::resolve_blackboard_resource(
                &self.arreio_mcp.blackboard,
                parts[0],
                parts[1],
            )? {
                Some(v) => Ok(v),
                None => bail!("tupla não encontrada: {}::{}", parts[0], parts[1]),
            }
        } else if let Some(task_id) = uri.strip_prefix("dag://") {
            let dag = arreio_dag::Dag::load(self.arreio_mcp.blackboard.clone())?;
            match arreio_mcp_server::resolve_dag_resource(&dag, task_id)? {
                Some(v) => Ok(v),
                None => bail!("tarefa não encontrada: {}", task_id),
            }
        } else if let Some(actor_id) = uri.strip_prefix("fsm://") {
            Ok(arreio_mcp_server::resolve_fsm_resource(
                &self.arreio_mcp.fsm,
                actor_id,
            )?)
        } else {
            bail!("esquema de URI não suportado: {}", uri)
        }
    }

    fn get_prompt(&self, name: &str) -> Result<String> {
        match name {
            "planning" => Ok(arreio_mcp_server::planning_prompt()),
            "review" => Ok(arreio_mcp_server::review_prompt()),
            "security_audit" => Ok(arreio_mcp_server::security_audit_prompt()),
            _ => bail!("prompt desconhecido: {}", name),
        }
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

// ═══════════════════════════════════════════════════════════════════════════════
// ClaudeHeadlessWrapper
// ═══════════════════════════════════════════════════════════════════════════════

/// Orquestra `claude -p <prompt>` via Hypervisor.
pub struct ClaudeHeadlessWrapper {
    hypervisor: arreio_hypervisor::Hypervisor,
}

impl ClaudeHeadlessWrapper {
    pub fn new() -> Self {
        Self {
            hypervisor: arreio_hypervisor::Hypervisor::new(60),
        }
    }

    /// Executa `claude -p <prompt>` e captura stdout.
    pub fn run(&self, prompt: &str) -> Result<String> {
        let escaped = prompt.replace('"', "\\\"");
        let cmd = format!("claude -p \"{}\"", escaped);
        let result = self.hypervisor.run(&cmd, None)?;
        Ok(result.stdout.trim_end().to_string())
    }

    /// Expõe o comando construído para facilitar testes unitários.
    #[cfg(test)]
    fn build_cmd(prompt: &str) -> String {
        let escaped = prompt.replace('"', "\\\"");
        format!("claude -p \"{}\"", escaped)
    }
}

impl Default for ClaudeHeadlessWrapper {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ClaudeMdImporter
// ═══════════════════════════════════════════════════════════════════════════════

/// Importa hierarchy CLAUDE.md para tuplas Blackboard.
/// Parseia headers markdown (#, ##, ###, etc.) como hierarquia de chaves separadas por `::`.
/// O conteúdo entre headers é armazenado como string JSON no Blackboard.
/// Retorna vetor de (chave_hierarquica, conteúdo) inseridos.
pub fn import_claude_md(
    path: &str,
    blackboard: &mut arreio_kernel::Blackboard,
) -> Result<Vec<(String, String)>> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("falha ao ler {}", path))?;

    let header_re = Regex::new(r"^(#{1,6})\s+(.+)$").unwrap();

    let mut stack: Vec<(usize, String)> = Vec::new(); // (nível, título)
    let mut current_key: Option<String> = None;
    let mut current_content: Vec<String> = Vec::new();
    let mut results: Vec<(String, String)> = Vec::new();

    let flush =
        |key: &Option<String>, content: &mut Vec<String>, results: &mut Vec<(String, String)>| {
            if let Some(k) = key {
                let text = content.join("\n").trim().to_string();
                if !text.is_empty() {
                    results.push((k.clone(), text));
                }
            }
            content.clear();
        };

    for line in raw.lines() {
        if let Some(caps) = header_re.captures(line) {
            flush(&current_key, &mut current_content, &mut results);

            let level = caps[1].len();
            let title = caps[2].trim().to_string();

            // Remove da pilha níveis iguais ou maiores
            while let Some(&(l, _)) = stack.last() {
                if l >= level {
                    stack.pop();
                } else {
                    break;
                }
            }
            stack.push((level, title));

            let key = stack
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join("::");
            current_key = Some(key);
        } else if current_key.is_some() {
            current_content.push(line.to_string());
        }
    }

    flush(&current_key, &mut current_content, &mut results);

    // Persiste no Blackboard
    for (key, value) in &results {
        blackboard.put_tuple("claude_md", key, Value::String(value.clone()))?;
    }

    Ok(results)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Testes
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use std::io::Cursor;
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

    // ── 1. ClaudeMcpServer ────────────────────────────────────────────────────

    #[test]
    fn claude_mcp_server_new() {
        let srv = make_server();
        let _wrapper = ClaudeMcpServer::new(srv);
    }

    #[test]
    fn serve_stdio_responde_initialize() {
        let srv = make_server();
        let wrapper = ClaudeMcpServer::new(srv);

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let body = req.to_string();
        let input = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut output: Vec<u8> = Vec::new();

        wrapper
            .serve_stdio_with(Cursor::new(input), &mut output)
            .unwrap();
        let resp_str = String::from_utf8(output).unwrap();
        assert!(resp_str.contains("arreio-mcp"));
        assert!(resp_str.contains("2024-11-05"));
    }

    #[test]
    fn serve_stdio_responde_tools_list() {
        let srv = make_server();
        let wrapper = ClaudeMcpServer::new(srv);

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        });
        let body = req.to_string();
        let input = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut output: Vec<u8> = Vec::new();

        wrapper
            .serve_stdio_with(Cursor::new(input), &mut output)
            .unwrap();
        let resp_str = String::from_utf8(output).unwrap();
        assert!(resp_str.contains("blackboard_read"));
        assert!(resp_str.contains("safe_execute"));
    }

    #[test]
    fn serve_stdio_method_not_found() {
        let srv = make_server();
        let wrapper = ClaudeMcpServer::new(srv);

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "inexistente",
            "params": {}
        });
        let body = req.to_string();
        let input = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut output: Vec<u8> = Vec::new();

        wrapper
            .serve_stdio_with(Cursor::new(input), &mut output)
            .unwrap();
        let resp_str = String::from_utf8(output).unwrap();
        assert!(resp_str.contains("-32601"));
        assert!(resp_str.contains("Method not found"));
    }

    #[test]
    fn serve_stdio_tool_call_blackboard_read() {
        let srv = make_server();
        let mut bb = srv.blackboard.clone();
        arreio_mcp_server::blackboard_write_tool(&mut bb, "test", "k1", "\"valor-teste\"").unwrap();

        let wrapper = ClaudeMcpServer::new(srv);

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "blackboard_read",
                "arguments": {
                    "cat": "test",
                    "key": "k1"
                }
            }
        });
        let body = req.to_string();
        let input = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut output: Vec<u8> = Vec::new();

        wrapper
            .serve_stdio_with(Cursor::new(input), &mut output)
            .unwrap();
        let resp_str = String::from_utf8(output).unwrap();
        assert!(resp_str.contains("valor-teste"));
    }

    // ── 2. ClaudeHeadlessWrapper ──────────────────────────────────────────────

    #[test]
    fn claude_headless_wrapper_new() {
        let _w = ClaudeHeadlessWrapper::new();
    }

    #[test]
    fn claude_headless_build_cmd_escapa_aspas() {
        let cmd = ClaudeHeadlessWrapper::build_cmd(r#"diga "oi""#);
        assert!(cmd.contains(r#"diga \"oi\""#));
        assert!(cmd.starts_with("claude -p "));
    }

    // ── 3. ClaudeMdImporter ───────────────────────────────────────────────────

    #[test]
    fn import_claude_md_simples() {
        let raw = "# Visão Geral\n\nTexto da visão geral.\n";
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), raw).unwrap();

        let mut bb = temp_bb();
        let res = import_claude_md(f.path().to_str().unwrap(), &mut bb).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].0, "Visão Geral");
        assert!(res[0].1.contains("Texto da visão geral."));

        let v = bb.get_tuple("claude_md", "Visão Geral").unwrap();
        assert_eq!(v, Value::String("Texto da visão geral.".into()));
    }

    #[test]
    fn import_claude_md_hierarquia() {
        let raw = r#"# Root
Root content.
## Child A
Child A content.
### GrandChild
Grand content.
## Child B
Child B content.
"#;
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), raw).unwrap();

        let mut bb = temp_bb();
        let res = import_claude_md(f.path().to_str().unwrap(), &mut bb).unwrap();
        assert_eq!(res.len(), 4);
        assert!(res.iter().any(|(k, _)| k == "Root"));
        assert!(res.iter().any(|(k, _)| k == "Root::Child A"));
        assert!(res.iter().any(|(k, _)| k == "Root::Child A::GrandChild"));
        assert!(res.iter().any(|(k, _)| k == "Root::Child B"));
    }

    #[test]
    fn import_claude_md_vazio() {
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "").unwrap();

        let mut bb = temp_bb();
        let res = import_claude_md(f.path().to_str().unwrap(), &mut bb).unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn import_claude_md_sem_headers() {
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), "apenas texto livre\nsem headers\n").unwrap();

        let mut bb = temp_bb();
        let res = import_claude_md(f.path().to_str().unwrap(), &mut bb).unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn import_claude_md_persiste_multiplas_tuplas() {
        let raw = "# Sec1\nA\n# Sec2\nB\n";
        let f = NamedTempFile::new().unwrap();
        std::fs::write(f.path(), raw).unwrap();

        let mut bb = temp_bb();
        import_claude_md(f.path().to_str().unwrap(), &mut bb).unwrap();

        assert!(bb.get_tuple("claude_md", "Sec1").is_some());
        assert!(bb.get_tuple("claude_md", "Sec2").is_some());
    }
}
