use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Mensagem JSON-RPC 2.0 genérica.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcMessage<T> {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

/// Parâmetros de initialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: HashMap<String, serde_json::Value>,
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// Resultado de initialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeResult {
    pub protocol_version: String,
    pub capabilities: HashMap<String, serde_json::Value>,
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Representação de uma tool MCP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Chamada de tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Resultado de tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_initialize_request() {
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            method: Some("initialize".to_string()),
            params: Some(InitializeParams {
                protocol_version: "2024-11-05".to_string(),
                capabilities: HashMap::new(),
                client_info: ClientInfo {
                    name: "arreio".to_string(),
                    version: "0.2.0".to_string(),
                },
            }),
            result: None,
            error: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("2024-11-05"));
    }

    #[test]
    fn deserialize_tools_list_response() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    {
                        "name": "read_file",
                        "description": "Lê um arquivo do disco",
                        "input_schema": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"}
                            }
                        }
                    }
                ]
            }
        }"#;

        let msg: JsonRpcMessage<serde_json::Value> =
            serde_json::from_str(json).unwrap();
        assert_eq!(msg.jsonrpc, "2.0");
        assert_eq!(msg.id, Some(2));
        assert!(msg.result.is_some());

        let tools = msg
            .result
            .as_ref()
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "read_file");
    }

    #[test]
    fn deserialize_error_response() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 3,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        }"#;

        let msg: JsonRpcMessage<serde_json::Value> =
            serde_json::from_str(json).unwrap();
        assert_eq!(msg.jsonrpc, "2.0");
        assert!(msg.error.is_some());
        let err = msg.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn serialize_tool_call() {
        let call = McpToolCall {
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/tmp/test.txt"}),
        };

        let json = serde_json::to_string(&call).unwrap();
        assert!(json.contains("read_file"));
        assert!(json.contains("/tmp/test.txt"));
    }

    #[test]
    fn deserialize_tool_result() {
        let json = r#"{
            "content": [
                {
                    "type": "text",
                    "text": "Conteúdo do arquivo..."
                }
            ],
            "is_error": false
        }"#;

        let result: McpToolResult = serde_json::from_str(json).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].content_type, "text");
        assert_eq!(result.content[0].text, "Conteúdo do arquivo...");
    }

    #[test]
    fn mcp_tool_serialization() {
        let tool = McpTool {
            name: "execute_command".to_string(),
            description: "Executa um comando shell".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout": {"type": "number"}
                },
                "required": ["command"]
            }),
        };

        let json = serde_json::to_string(&tool).unwrap();
        let parsed: McpTool = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "execute_command");
        assert_eq!(parsed.description, "Executa um comando shell");
    }

    #[test]
    fn client_info_serialization() {
        let info = ClientInfo {
            name: "arreio".to_string(),
            version: "1.0.0".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("arreio"));
        assert!(json.contains("1.0.0"));
    }
}
