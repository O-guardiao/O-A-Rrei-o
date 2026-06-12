//! StreamingToolExecutor — GAP-002
//!
//! Executa ferramentas read-only especulativamente enquanto o stream
//! do LLM ainda está em andamento. Reduz latência percebida ao
//! sobrepor geração com execução.

use crate::{ToolRegistry, ToolRequest, ToolResult};
use arreio_provider::ToolCall;
use serde_json::Value;
use std::collections::HashMap;

/// Estado de parse incremental de tool calls.
pub struct StreamingToolExecutor {
    registry: ToolRegistry,
    pending_calls: Vec<ToolCall>,
    executed_ids: HashMap<String, ToolResult>,
    buffer: String,
}

impl StreamingToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            pending_calls: Vec::new(),
            executed_ids: HashMap::new(),
            buffer: String::new(),
        }
    }

    /// Processa um chunk de stream.
    /// Retorna resultados de ferramentas read-only que foram executadas imediatamente.
    pub fn on_stream_chunk(&mut self, chunk: &str) -> Vec<ToolResult> {
        self.buffer.push_str(chunk);

        // Tenta parsear tool calls parciais do buffer
        let new_calls = self.parse_partial_tool_calls(&self.buffer);
        let mut results = Vec::new();

        for call in new_calls {
            // Evita duplicatas
            if self.executed_ids.contains_key(&call.id) || self.pending_calls.iter().any(|c| c.id == call.id) {
                continue;
            }

            // Converte arguments string -> Value
            let args_value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);

            // Se é read-only, executa imediatamente
            if self.is_read_only(&call.function.name) {
                let req = ToolRequest {
                    name: call.function.name.clone(),
                    arguments: args_value,
                };
                let result = self.registry.call(req).unwrap_or_else(|e| ToolResult::err(format!("streaming exec error: {}", e)));
                self.executed_ids.insert(call.id.clone(), result.clone());
                results.push(result);
            } else {
                // Write: adiciona à lista pendente para execução serial após stream
                self.pending_calls.push(call);
            }
        }

        results
    }

    /// Chamado quando o stream completa.
    /// Executa todas as ferramentas write pendentes serialmente.
    pub fn on_stream_complete(&mut self) -> Vec<ToolResult> {
        let mut results = Vec::new();

        for call in &self.pending_calls {
            let args_value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
            let req = ToolRequest {
                name: call.function.name.clone(),
                arguments: args_value,
            };
            let result = self.registry.call(req).unwrap_or_else(|e| ToolResult::err(format!("streaming exec error: {}", e)));
            results.push(result);
        }

        // Limpa estado
        self.pending_calls.clear();
        self.buffer.clear();

        results
    }

    /// Aborta execuções pendentes e limpa estado.
    pub fn abort(&mut self) {
        self.pending_calls.clear();
        self.buffer.clear();
        self.executed_ids.clear();
    }

    /// Número de chamadas de ferramentas write pendentes.
    pub fn pending_calls_count(&self) -> usize {
        self.pending_calls.len()
    }

    /// Verifica se uma ferramenta é read-only.
    fn is_read_only(&self, name: &str) -> bool {
        let read_only_tools = [
            "read_file",
            "grep_search",
            "glob_search",
            "list_dir",
            "memory_search",
            "web_search",
            "web_fetch",
            "describe_image",
            "transcribe_audio",
        ];
        read_only_tools.contains(&name)
    }

    /// Parse incremental de tool calls parciais.
    /// Tenta extrair tool calls do buffer mesmo que incompletos.
    fn parse_partial_tool_calls(&self, buffer: &str) -> Vec<ToolCall> {
        let mut calls = Vec::new();

        // Padrão 1: JSON array de tool calls
        if let Some(start) = buffer.find('[') {
            if let Some(end) = buffer.rfind(']') {
                let json_str = &buffer[start..=end];
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                    for (i, v) in arr.iter().enumerate() {
                        if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                            let args = v
                                .get("arguments")
                                .map(|a| a.to_string())
                                .unwrap_or_else(|| "{}".to_string());
                            let id = format!("stream_{}", i);
                            if !calls.iter().any(|c: &ToolCall| c.id == id) {
                                calls.push(ToolCall {
                                    id,
                                    r#type: "function".to_string(),
                                    function: arreio_provider::ToolCallFunction {
                                        name: name.to_string(),
                                        arguments: args,
                                    },
                                });
                            }
                        }
                    }
                }
            }
        }

        // Padrão 2: Markdown fences com JSON
        if buffer.contains("```json") {
            let clean = extract_json_from_markdown(buffer);
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&clean) {
                for (i, v) in arr.iter().enumerate() {
                    if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                        let args = v
                            .get("arguments")
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| "{}".to_string());
                        let id = format!("stream_md_{}", i);
                        if !calls.iter().any(|c: &ToolCall| c.id == id) {
                            calls.push(ToolCall {
                                id,
                                r#type: "function".to_string(),
                                function: arreio_provider::ToolCallFunction {
                                    name: name.to_string(),
                                    arguments: args,
                                },
                            });
                        }
                    }
                }
            }
        }

        calls
    }
}

/// Extrai JSON de blocos markdown ```json ... ```.
fn extract_json_from_markdown(text: &str) -> String {
    if let Some(start) = text.find("```json") {
        let inner = &text[start + 7..];
        if let Some(end) = inner.find("```") {
            return inner[..end].trim().to_string();
        }
    }
    if let Some(start) = text.find("```") {
        let inner = &text[start + 3..];
        if let Some(end) = inner.find("```") {
            return inner[..end].trim().to_string();
        }
    }
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_registry() -> ToolRegistry {
        // ToolRegistry vazio para testes — em produção seria populado
        ToolRegistry::new()
    }

    #[test]
    fn streaming_executor_dispatches_read_during_stream() {
        let registry = mock_registry();
        let mut executor = StreamingToolExecutor::new(registry);
        let chunk = r#"[
  {"name": "read_file", "arguments": {"path": "src/main.rs"}}
]"#;
        let results = executor.on_stream_chunk(chunk);
        // read_file é read-only, mas registry vazio retorna erro — ainda assim testa o dispatch
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn write_tools_queued_not_executed_during_stream() {
        let registry = mock_registry();
        let mut executor = StreamingToolExecutor::new(registry);
        let chunk = r#"[
  {"name": "write_file", "arguments": {"path": "out.txt", "content": "hello"}}
]"#;
        let results = executor.on_stream_chunk(chunk);
        // write_file não é read-only → não deve executar durante stream
        assert!(results.is_empty());
        // Deve estar na fila de pendentes
        assert_eq!(executor.pending_calls.len(), 1);
    }

    #[test]
    fn stream_complete_executes_writes() {
        let registry = mock_registry();
        let mut executor = StreamingToolExecutor::new(registry);
        let chunk = r#"[
  {"name": "write_file", "arguments": {"path": "out.txt", "content": "hello"}}
]"#;
        executor.on_stream_chunk(chunk);
        let results = executor.on_stream_complete();
        assert_eq!(results.len(), 1);
        assert!(executor.pending_calls.is_empty());
    }

    #[test]
    fn abort_clears_state() {
        let registry = mock_registry();
        let mut executor = StreamingToolExecutor::new(registry);
        executor.buffer = "some partial data".to_string();
        executor.pending_calls.push(ToolCall {
            id: "t1".to_string(),
            r#type: "function".to_string(),
            function: arreio_provider::ToolCallFunction {
                name: "write_file".to_string(),
                arguments: "{}".to_string(),
            },
        });
        executor.abort();
        assert!(executor.buffer.is_empty());
        assert!(executor.pending_calls.is_empty());
    }
}
