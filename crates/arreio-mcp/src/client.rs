use crate::protocol::*;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Cliente MCP (Model Context Protocol) sobre stdio.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    id_counter: AtomicU64,
}

impl McpClient {
    /// Spawna um servidor MCP (ex: `npx -y @anthropics/anthropic-mcp` ou outro).
    pub fn spawn(command: &str, args: &[&str]) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("falha ao spawnar MCP server: {}", command))?;

        let stdin = child.stdin.take().context("stdin não disponível")?;
        Ok(Self {
            child,
            stdin,
            id_counter: AtomicU64::new(1),
        })
    }

    /// Handshake initialize.
    pub fn initialize(&mut self) -> Result<McpInitializeResult> {
        let params = InitializeParams {
            protocol_version: "2024-11-05".into(),
            capabilities: Default::default(),
            client_info: ClientInfo {
                name: "arreio".into(),
                version: "0.2.0".into(),
            },
        };
        let res = self.request("initialize", Some(serde_json::to_value(params)?))?;
        serde_json::from_value(res).context("falha ao parsear initialize result")
    }

    /// Lista tools disponíveis.
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let res = self.request("tools/list", None)?;
        let arr = res
            .get("tools")
            .and_then(|v| v.as_array())
            .context("campo 'tools' ausente")?;
        arr.iter()
            .map(|v| serde_json::from_value(v.clone()).context("parse McpTool"))
            .collect()
    }

    /// Chama uma tool.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<McpToolResult> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });
        let res = self.request("tools/call", Some(params))?;
        serde_json::from_value(res).context("parse McpToolResult")
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.id_counter.fetch_add(1, Ordering::SeqCst);
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: Some(method.into()),
            params,
            result: None,
            error: None,
        };
        let json = serde_json::to_string(&msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", json.len());

        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(json.as_bytes())?;
        self.stdin.flush()?;

        let stdout = self
            .child
            .stdout
            .as_mut()
            .context("stdout não disponível")?;
        let mut reader = BufReader::new(stdout);

        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line.trim().is_empty() {
                break;
            }
            if line.to_lowercase().starts_with("content-length:") {
                content_length = line.split(':').nth(1).and_then(|s| s.trim().parse().ok());
            }
        }

        let len = content_length.context("Content-Length ausente")?;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)?;

        let resp: JsonRpcMessage<Value> = serde_json::from_slice(&buf)?;
        if let Some(err) = resp.error {
            bail!("MCP error ({}): {}", err.code, err.message);
        }
        resp.result.context("resposta sem result")
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.request("notifications/initialized", None);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Gera uma mensagem JSON-RPC como seria enviada pelo cliente.
    fn build_request_json(method: &str, params: Option<Value>, id: u64) -> String {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".to_string(),
            id: Some(id),
            method: Some(method.to_string()),
            params,
            result: None,
            error: None,
        };
        serde_json::to_string(&msg).unwrap()
    }

    #[test]
    fn request_initialize_format() {
        let params = serde_json::json!({
            "protocol_version": "2024-11-05",
            "capabilities": {},
            "client_info": {"name": "arreio", "version": "0.2.0"}
        });
        let json = build_request_json("initialize", Some(params), 1);

        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("2024-11-05"));
        assert!(json.contains("arreio"));
    }

    #[test]
    fn request_tools_list_format() {
        let json = build_request_json("tools/list", None, 2);

        assert!(json.contains("\"method\":\"tools/list\""));
        assert!(json.contains("\"id\":2"));
        // params deve estar ausente (None)
        assert!(!json.contains("\"params\":null"));
    }

    #[test]
    fn request_tool_call_format() {
        let params = serde_json::json!({
            "name": "read_file",
            "arguments": {"path": "/tmp/test.txt"}
        });
        let json = build_request_json("tools/call", Some(params), 3);

        assert!(json.contains("\"method\":\"tools/call\""));
        assert!(json.contains("\"id\":3"));
        assert!(json.contains("read_file"));
        assert!(json.contains("/tmp/test.txt"));
    }

    #[test]
    fn parse_success_response() {
        let resp_json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocol_version": "2024-11-05",
                "capabilities": {},
                "server_info": {"name": "test-server", "version": "1.0"}
            }
        }"#;

        let resp: JsonRpcMessage<Value> = serde_json::from_str(resp_json).unwrap();
        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn parse_error_response() {
        let resp_json = r#"{
            "jsonrpc": "2.0",
            "id": 42,
            "error": {
                "code": -32000,
                "message": "Server error"
            }
        }"#;

        let resp: JsonRpcMessage<Value> = serde_json::from_str(resp_json).unwrap();
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32000);
        assert!(err.message.contains("Server error"));
    }

    #[test]
    fn content_length_header_format() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        assert!(header.starts_with("Content-Length: "));
        assert_eq!(
            header,
            format!("Content-Length: {}\r\n\r\n", body.len())
        );
    }

    #[test]
    fn mcptool_deserialize() {
        let json = r#"{
            "name": "search",
            "description": "Busca arquivos",
            "input_schema": {"type": "object", "properties": {"query": {"type": "string"}}}
        }"#;
        let tool: McpTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "search");
        assert_eq!(tool.description, "Busca arquivos");
        assert!(tool.input_schema.is_object());
    }

    #[test]
    fn mcptoolresult_deserialize() {
        let json = r#"{
            "content": [
                {"type": "text", "text": "resultado da busca"}
            ],
            "is_error": false
        }"#;
        let result: McpToolResult = serde_json::from_str(json).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text, "resultado da busca");
    }
}
