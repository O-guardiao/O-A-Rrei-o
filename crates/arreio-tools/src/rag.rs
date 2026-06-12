//! Tools RAG — chunk_document, embed_texts, vector_search (PVC-Q2.3).
//!
//! RAG como serviço explícito: as tools são registradas no ToolRegistry e
//! invocadas quando o PLANNER decide — nunca como augmentation automática
//! de prompt. A ingestão (insert) é primitiva do kernel
//! (`bb.vector_insert`); a consulta combinada com grafo está em
//! `arreio_memory::GraphRagPipeline`.

use crate::{ToolHandler, ToolRequest, ToolResult};
use anyhow::Result;
use arreio_kernel::Blackboard;
use arreio_memory::{Chunker, FixedSizeChunker, MarkdownChunker};
use arreio_provider::{ProviderClient, ToolDescriptor, ToolFunction};

// ── chunk_document ────────────────────────────────────────────────────────────

/// Divide um documento em chunks (fixed-size ou markdown-aware).
pub struct ChunkDocumentTool;

impl ChunkDocumentTool {
    pub fn new() -> Self {
        Self
    }

    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "chunk_document".to_string(),
                description: "Divide um documento em chunks para indexação RAG. \
                              Estratégias: 'fixed' (tamanho fixo com overlap) ou \
                              'markdown' (respeita headers)."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "Documento a dividir"},
                        "strategy": {"type": "string", "enum": ["fixed", "markdown"], "description": "Estratégia de chunking (padrão: fixed)"},
                        "chunk_size": {"type": "integer", "description": "Tamanho do chunk em caracteres (padrão: 512)"},
                        "overlap": {"type": "integer", "description": "Overlap entre chunks no modo fixed (padrão: 64)"}
                    },
                    "required": ["text"]
                }),
            },
        }
    }
}

impl Default for ChunkDocumentTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolHandler for ChunkDocumentTool {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let text = request
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if text.is_empty() {
            return Ok(ToolResult::err("chunk_document requer argumento 'text'"));
        }
        let strategy = request
            .arguments
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("fixed");
        let chunk_size = request
            .arguments
            .get("chunk_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(512) as usize;
        let overlap = request
            .arguments
            .get("overlap")
            .and_then(|v| v.as_u64())
            .unwrap_or(64) as usize;

        if chunk_size == 0 {
            return Ok(ToolResult::err("chunk_size deve ser > 0"));
        }

        let chunks = match strategy {
            "markdown" => MarkdownChunker::new(chunk_size).chunk(text),
            "fixed" => {
                // overlap só se aplica ao modo fixed.
                if overlap >= chunk_size {
                    return Ok(ToolResult::err("overlap deve ser < chunk_size"));
                }
                FixedSizeChunker::new(chunk_size, overlap).chunk(text)
            }
            other => {
                return Ok(ToolResult::err(format!(
                    "estratégia desconhecida: '{}' (use 'fixed' ou 'markdown')",
                    other
                )))
            }
        };

        Ok(ToolResult::ok(serde_json::to_string(&chunks)?))
    }
}

// ── embed_texts ───────────────────────────────────────────────────────────────

/// Gera embeddings via ProviderClient (Ollama/OpenAI/etc.).
pub struct EmbedTextsTool {
    client: Box<dyn ProviderClient>,
}

impl EmbedTextsTool {
    pub fn new(client: Box<dyn ProviderClient>) -> Self {
        Self { client }
    }

    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "embed_texts".to_string(),
                description: "Gera embeddings vetoriais para uma lista de textos usando \
                              o provider configurado."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "texts": {"type": "array", "items": {"type": "string"}, "description": "Textos a embeddar"}
                    },
                    "required": ["texts"]
                }),
            },
        }
    }
}

impl ToolHandler for EmbedTextsTool {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let texts: Vec<String> = request
            .arguments
            .get("texts")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if texts.is_empty() {
            return Ok(ToolResult::err(
                "embed_texts requer argumento 'texts' (array de strings não vazio)",
            ));
        }
        match self.client.embed(texts) {
            Ok(embeddings) => Ok(ToolResult::ok(serde_json::to_string(&embeddings)?)),
            Err(e) => Ok(ToolResult::err(format!("embedding falhou: {}", e))),
        }
    }
}

// ── vector_search ─────────────────────────────────────────────────────────────

/// Busca semântica no vector store do Blackboard, com expansão GraphRAG
/// opcional (`max_hops > 0`).
pub struct VectorSearchTool {
    blackboard: Blackboard,
    client: Box<dyn ProviderClient>,
}

impl VectorSearchTool {
    pub fn new(blackboard: Blackboard, client: Box<dyn ProviderClient>) -> Self {
        Self { blackboard, client }
    }

    pub fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: ToolFunction {
                name: "vector_search".to_string(),
                description: "Busca semântica no vector store do Blackboard. Embedda a \
                              query e retorna os top_k chunks mais similares; com \
                              max_hops > 0 expande o resultado pelo GraphStore (GraphRAG)."
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Texto da consulta"},
                        "top_k": {"type": "integer", "description": "Número de resultados (padrão: 5)"},
                        "max_hops": {"type": "integer", "description": "Hops de expansão no grafo (padrão: 0 = sem GraphRAG)"}
                    },
                    "required": ["query"]
                }),
            },
        }
    }
}

impl ToolHandler for VectorSearchTool {
    fn handle(&self, request: ToolRequest) -> Result<ToolResult> {
        let query = request
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if query.is_empty() {
            return Ok(ToolResult::err("vector_search requer argumento 'query'"));
        }
        let top_k = request
            .arguments
            .get("top_k")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;
        let max_hops = request
            .arguments
            .get("max_hops")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let embedding = match self.client.embed(vec![query.to_string()]) {
            Ok(mut e) if !e.is_empty() => e.remove(0),
            Ok(_) => return Ok(ToolResult::err("provider retornou embedding vazio")),
            Err(e) => return Ok(ToolResult::err(format!("embedding da query falhou: {}", e))),
        };

        if max_hops > 0 {
            let pipeline = arreio_memory::GraphRagPipeline::new(self.blackboard.clone());
            match pipeline.query(&embedding, top_k, max_hops) {
                Ok(results) => Ok(ToolResult::ok(serde_json::to_string(&results)?)),
                Err(e) => Ok(ToolResult::err(format!("GraphRAG falhou: {}", e))),
            }
        } else {
            let hits = self.blackboard.vector_query(&embedding, top_k);
            Ok(ToolResult::ok(serde_json::to_string(&hits)?))
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

    #[test]
    fn chunk_document_fixed() {
        let tool = ChunkDocumentTool::new();
        let result = tool
            .handle(ToolRequest {
                name: "chunk_document".into(),
                arguments: serde_json::json!({
                    "text": "a".repeat(1000),
                    "chunk_size": 400,
                    "overlap": 50
                }),
            })
            .unwrap();
        assert!(result.success);
        let chunks: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn chunk_document_markdown() {
        let tool = ChunkDocumentTool::new();
        let result = tool
            .handle(ToolRequest {
                name: "chunk_document".into(),
                arguments: serde_json::json!({
                    "text": "# Titulo\ntexto um\n\n# Outro\ntexto dois",
                    "strategy": "markdown",
                    "chunk_size": 64
                }),
            })
            .unwrap();
        assert!(result.success);
        let chunks: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert!(!chunks.is_empty());
    }

    #[test]
    fn chunk_document_valida_parametros() {
        let tool = ChunkDocumentTool::new();
        let result = tool
            .handle(ToolRequest {
                name: "chunk_document".into(),
                arguments: serde_json::json!({"text": "abc", "chunk_size": 10, "overlap": 10}),
            })
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn embed_texts_via_mock() {
        let tool = EmbedTextsTool::new(Box::new(MockProvider::new("ok")));
        let result = tool
            .handle(ToolRequest {
                name: "embed_texts".into(),
                arguments: serde_json::json!({"texts": ["gato", "cachorro"]}),
            })
            .unwrap();
        assert!(result.success);
        let embeddings: Vec<Vec<f32>> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), 4); // mock retorna 4 dims
    }

    #[test]
    fn embed_texts_exige_lista() {
        let tool = EmbedTextsTool::new(Box::new(MockProvider::new("ok")));
        let result = tool
            .handle(ToolRequest {
                name: "embed_texts".into(),
                arguments: serde_json::json!({}),
            })
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn vector_search_fluxo_completo() {
        let bb = temp_bb();
        let mock = MockProvider::new("ok");
        // Mock embed: vetor [len, len/2, len/4, len/8] — "gato" (4 chars)
        // e a query "gato" produzem o MESMO vetor → score 1.0.
        let emb = mock.embed(vec!["gato".to_string()]).unwrap().remove(0);
        bb.vector_insert("c1", "gato", emb, serde_json::json!({"doc": "animais"}))
            .unwrap();
        let emb2 = mock
            .embed(vec!["um texto bem mais longo sobre carros".to_string()])
            .unwrap()
            .remove(0);
        bb.vector_insert("c2", "carros", emb2, serde_json::json!(null))
            .unwrap();

        let tool = VectorSearchTool::new(bb, Box::new(mock));
        let result = tool
            .handle(ToolRequest {
                name: "vector_search".into(),
                arguments: serde_json::json!({"query": "gato", "top_k": 1}),
            })
            .unwrap();
        assert!(result.success);
        let hits: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "c1");
    }

    #[test]
    fn vector_search_com_graphrag() {
        let bb = temp_bb();
        let mock = MockProvider::new("ok");
        let emb = mock.embed(vec!["gato".to_string()]).unwrap().remove(0);
        bb.vector_insert("c1", "gato", emb, serde_json::json!(null))
            .unwrap();
        let graph = arreio_memory::GraphStore::new(bb.clone());
        graph
            .add_relation(&arreio_memory::graph::Relation {
                subject: "c1".into(),
                predicate: "depends_on".into(),
                object: "c9".into(),
                confidence: 1.0,
            })
            .unwrap();

        let tool = VectorSearchTool::new(bb, Box::new(mock));
        let result = tool
            .handle(ToolRequest {
                name: "vector_search".into(),
                arguments: serde_json::json!({"query": "gato", "top_k": 1, "max_hops": 1}),
            })
            .unwrap();
        assert!(result.success);
        let results: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(results.len(), 2); // c1 (vector) + c9 (graph)
    }

    #[test]
    fn descriptors_validos() {
        assert_eq!(
            ChunkDocumentTool::descriptor().function.name,
            "chunk_document"
        );
        assert_eq!(EmbedTextsTool::descriptor().function.name, "embed_texts");
        assert_eq!(
            VectorSearchTool::descriptor().function.name,
            "vector_search"
        );
    }
}
