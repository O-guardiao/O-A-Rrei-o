//! Arreio-Bridge-Hermes — Adapter para Hermes Agent (Nous Research).
//!
//! Expõe O Arreio como API server OpenAI-compatible para Hermes conectar como backend.
//! Importa Skill Store (texto) e converte para AST structures/tuplas via arreio-ast.
//! Exporta skills O Arreio como "modelos" no Hermes.

use anyhow::{Context, Result};
use arreio_provider::{ChatRequest, ProviderClient, ProviderPool, ToolCall};
use arreio_skills::{Skill, SkillMd, SkillStore, SkillTrust};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

// ===================================================================
// HermesApiServer
// ===================================================================

/// API server OpenAI-compatible para Hermes.
pub struct HermesApiServer {
    provider_pool: ProviderPool,
    port: u16,
}

impl HermesApiServer {
    pub fn new(provider_pool: ProviderPool, port: u16) -> Self {
        Self {
            provider_pool,
            port,
        }
    }

    /// Serve HTTP OpenAI-compatible na porta configurada.
    ///
    /// Endpoints:
    /// - GET /v1/models → lista modelos disponíveis no pool
    /// - POST /v1/chat/completions → delega para ProviderPool
    /// - POST /v1/embeddings → delega para ProviderPool.embed()
    pub fn serve(&self) -> Result<()> {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener =
            TcpListener::bind(&addr).with_context(|| format!("falha ao bind em {}", addr))?;
        println!("[hermes-bridge] ouvindo em http://{}", addr);

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(e) = handle_connection(stream, &self.provider_pool) {
                        eprintln!("[hermes-bridge] erro na conexão: {}", e);
                    }
                }
                Err(e) => eprintln!("[hermes-bridge] erro de aceite: {}", e),
            }
        }
        Ok(())
    }
}

// -------------------------------------------------------------------
// HTTP helpers
// -------------------------------------------------------------------

fn handle_connection(mut stream: TcpStream, pool: &ProviderPool) -> Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut reader = BufReader::new(&stream);

    // Lê a primeira linha: METHOD PATH HTTP/1.1
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let parts: Vec<&str> = first_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        return send_json_response(&mut stream, 400, r#"{"error":"bad request"}"#);
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
        ("GET", "/v1/models") => handle_models(&mut stream, pool),
        ("POST", "/v1/chat/completions") => handle_chat_completions(&mut stream, pool, &body),
        ("POST", "/v1/embeddings") => handle_embeddings(&mut stream, pool, &body),
        _ => send_json_response(&mut stream, 404, r#"{"error":"not found"}"#),
    }
}

fn send_json_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n\
         {}",
        status,
        status_text,
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

// -------------------------------------------------------------------
// Endpoints
// -------------------------------------------------------------------

fn handle_models(stream: &mut TcpStream, pool: &ProviderPool) -> Result<()> {
    let names = pool.provider_names();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let data: Vec<ModelInfo> = names
        .into_iter()
        .map(|name| ModelInfo {
            id: name,
            object: "model".to_string(),
            created: now,
            owned_by: "arreio".to_string(),
        })
        .collect();

    let resp = ModelsResponse {
        object: "list".to_string(),
        data,
    };
    let json = serde_json::to_string(&resp)?;
    send_json_response(stream, 200, &json)
}

fn handle_chat_completions(stream: &mut TcpStream, pool: &ProviderPool, body: &str) -> Result<()> {
    let req: ChatCompletionRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return send_json_response(stream, 400, &format!(r#"{{"error":"{}"}}"#, e));
        }
    };

    if req.stream {
        return send_json_response(stream, 400, r#"{"error":"streaming not supported"}"#);
    }

    let mut system = String::new();
    let mut user = String::new();
    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(&msg.content);
            }
            "user" => {
                if !user.is_empty() {
                    user.push('\n');
                }
                user.push_str(&msg.content);
            }
            _ => {}
        }
    }

    let chat_req = ChatRequest {
        messages: Vec::new(),
        model: req.model.clone(),
        system,
        user,
        tools: req.tools,
    };

    match pool.chat(chat_req) {
        Ok(resp) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());

            let json_resp = ChatCompletionResponse {
                id,
                object: "chat.completion".to_string(),
                created: now,
                model: req.model,
                choices: vec![Choice {
                    index: 0,
                    message: ResponseMessage {
                        role: "assistant".to_string(),
                        content: resp.content,
                        tool_calls: resp.tool_calls,
                    },
                    finish_reason: "stop".to_string(),
                }],
                usage: Usage {
                    prompt_tokens: resp.tokens_in,
                    completion_tokens: resp.tokens_out,
                    total_tokens: resp.tokens_in + resp.tokens_out,
                },
            };
            let json = serde_json::to_string(&json_resp)?;
            send_json_response(stream, 200, &json)
        }
        Err(e) => {
            let err = format!(r#"{{"error":"{}"}}"#, e);
            send_json_response(stream, 502, &err)
        }
    }
}

fn handle_embeddings(stream: &mut TcpStream, pool: &ProviderPool, body: &str) -> Result<()> {
    let req: EmbeddingsRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return send_json_response(stream, 400, &format!(r#"{{"error":"{}"}}"#, e));
        }
    };

    let texts = match req.input {
        EmbeddingsInput::Single(s) => vec![s],
        EmbeddingsInput::Multiple(v) => v,
    };

    match pool.embed(texts.clone()) {
        Ok(embeddings) => {
            let data: Vec<EmbeddingData> = embeddings
                .into_iter()
                .enumerate()
                .map(|(i, emb)| EmbeddingData {
                    object: "embedding".to_string(),
                    embedding: emb,
                    index: i as u32,
                })
                .collect();

            let prompt_tokens: u32 = texts
                .iter()
                .map(|t| t.split_whitespace().count() as u32)
                .sum();
            let resp = EmbeddingsResponse {
                object: "list".to_string(),
                data,
                model: req.model,
                usage: EmbeddingUsage {
                    prompt_tokens,
                    total_tokens: prompt_tokens,
                },
            };
            let json = serde_json::to_string(&resp)?;
            send_json_response(stream, 200, &json)
        }
        Err(e) => {
            let err = format!(r#"{{"error":"{}"}}"#, e);
            send_json_response(stream, 502, &err)
        }
    }
}

// -------------------------------------------------------------------
// OpenAI-compatible JSON structs
// -------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(default)]
    tools: Option<Vec<arreio_provider::ToolDescriptor>>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct EmbeddingsRequest {
    model: String,
    input: EmbeddingsInput,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum EmbeddingsInput {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    object: String,
    data: Vec<ModelInfo>,
}

#[derive(Debug, Serialize)]
struct ModelInfo {
    id: String,
    object: String,
    created: u64,
    owned_by: String,
}

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Debug, Serialize)]
struct Choice {
    index: u32,
    message: ResponseMessage,
    finish_reason: String,
}

#[derive(Debug, Serialize)]
struct ResponseMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Serialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Serialize)]
struct EmbeddingsResponse {
    object: String,
    data: Vec<EmbeddingData>,
    model: String,
    usage: EmbeddingUsage,
}

#[derive(Debug, Serialize)]
struct EmbeddingData {
    object: String,
    embedding: Vec<f32>,
    index: u32,
}

#[derive(Debug, Serialize)]
struct EmbeddingUsage {
    prompt_tokens: u32,
    total_tokens: u32,
}

// ===================================================================
// SkillStoreImporter
// ===================================================================

/// Importa Skill Store do Hermes (texto) para AST/tuplas.
///
/// Aceita um arquivo único ou um diretório contendo vários arquivos.
/// Cada arquivo deve estar no formato `SkillMd` (YAML frontmatter + Markdown).
pub fn import_hermes_skills(path: &str, skills: &mut SkillStore) -> Result<Vec<String>> {
    let path = Path::new(path);
    let mut imported = Vec::new();

    if path.is_file() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("falha ao ler arquivo {}", path.display()))?;
        let name = import_single_skill(&content, skills)?;
        imported.push(name);
    } else if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let p = entry.path();
            if p.is_file() {
                let content = std::fs::read_to_string(&p)
                    .with_context(|| format!("falha ao ler arquivo {}", p.display()))?;
                let name = import_single_skill(&content, skills)?;
                imported.push(name);
            }
        }
    } else {
        anyhow::bail!(
            "caminho não existe ou não é arquivo/diretório: {}",
            path.display()
        );
    }

    Ok(imported)
}

fn import_single_skill(content: &str, skills: &mut SkillStore) -> Result<String> {
    let skill_md =
        SkillMd::parse(content).with_context(|| "falha ao parsear SKILL.md do Hermes")?;

    let ast_signature = extract_ast_from_body(&skill_md.body);

    let skill = Skill {
        name: skill_md.name.clone(),
        description: skill_md.description,
        trigger_patterns: skill_md.tags.clone(),
        ast_signature,
        file_target_pattern: None,
        instruction_template: skill_md.when_to_use,
        steps: skill_md.prerequisites,
        templates: {
            let mut m = HashMap::new();
            if !skill_md.quick_reference.is_empty() {
                m.insert("quick_reference".into(), skill_md.quick_reference);
            }
            if !skill_md.examples.is_empty() {
                m.insert("examples".into(), skill_md.examples);
            }
            m
        },
        validation_cmds: Vec::new(),
        last_used: 0,
        usage_count: 0,
        success_rate: 0.0,
        created_from_dag_task_id: None,
        anti_conversation: true,
        idempotent: false,
        error_budget: 3,
        output_schema: None,
        allowed_tools: vec![],
        trust_level: SkillTrust::Untrusted,
        module_count: 1,
        mutation_history: vec![],
    };

    skills.save(&skill)?;
    Ok(skill_md.name)
}

/// Extrai AST signature de blocos de código Rust no corpo da skill.
fn extract_ast_from_body(body: &str) -> Option<String> {
    let mut code = String::new();
    let mut in_rust = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```rust") {
            in_rust = true;
            continue;
        }
        if trimmed.starts_with("```") && in_rust {
            in_rust = false;
            continue;
        }
        if in_rust {
            code.push_str(line);
            code.push('\n');
        }
    }

    if code.trim().is_empty() {
        return None;
    }

    match arreio_ast::extract_from_str(&code, "skill.rs") {
        Ok(map) => Some(map.to_compact_json()),
        Err(_) => {
            let map = arreio_ast::extract_generic(&code, "skill.rs");
            Some(map.to_compact_json())
        }
    }
}

// ===================================================================
// SkillExporter
// ===================================================================

/// Exporta skills O Arreio como "modelos" no formato Hermes.
///
/// Retorna um JSON no formato OpenAI `/v1/models`.
pub fn export_skills(skills: &SkillStore) -> Result<String> {
    let list = skills.list();
    let data: Vec<ModelInfo> = list
        .into_iter()
        .map(|skill| ModelInfo {
            id: skill.name,
            object: "model".to_string(),
            created: skill.last_used,
            owned_by: "arreio".to_string(),
        })
        .collect();

    let resp = ModelsResponse {
        object: "list".to_string(),
        data,
    };

    serde_json::to_string_pretty(&resp).context("falha ao serializar skills exportadas")
}

// ===================================================================
// Testes
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::Blackboard;
    use arreio_provider::{FailoverStrategy, MockProvider};
    use std::io::Read;
    use std::net::TcpListener;
    use tempfile::NamedTempFile;

    fn temp_bb() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let p: std::path::PathBuf = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&p).unwrap()
    }

    fn make_pool() -> ProviderPool {
        let mock = MockProvider::new("resposta padrão");
        ProviderPool::new(FailoverStrategy::Priority).add_provider(Box::new(mock))
    }

    /// Helper: envia uma requisição HTTP raw para handle_connection e retorna a resposta raw.
    fn test_request(pool: &ProviderPool, request: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let request = request.to_string();

        let client_thread = std::thread::spawn(move || {
            let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            client.write_all(request.as_bytes()).unwrap();
            client.shutdown(std::net::Shutdown::Write).unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            response
        });

        let (stream, _) = listener.accept().unwrap();
        handle_connection(stream, pool).unwrap();

        client_thread.join().unwrap()
    }

    #[test]
    fn api_server_port() {
        let pool = make_pool();
        let server = HermesApiServer::new(pool, 9876);
        assert_eq!(server.port, 9876);
    }

    #[test]
    fn models_endpoint() {
        let pool = make_pool();
        let response = test_request(&pool, "GET /v1/models HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(response.contains("200 OK"));
        assert!(response.contains("mock"));
    }

    #[test]
    fn chat_completions_endpoint() {
        let pool = make_pool();
        let body = r#"{
            "model": "mock",
            "messages": [
                {"role": "system", "content": "Você é um assistente."},
                {"role": "user", "content": "Olá"}
            ]
        }"#;
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );
        let response = test_request(&pool, &request);
        assert!(response.contains("200 OK"));
        assert!(response.contains("assistant"));
        assert!(response.contains("resposta padrão"));
    }

    #[test]
    fn embeddings_endpoint() {
        let pool = make_pool();
        let body = r#"{
            "model": "mock",
            "input": "hello world"
        }"#;
        let request = format!(
            "POST /v1/embeddings HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );
        let response = test_request(&pool, &request);
        assert!(response.contains("200 OK"));
        assert!(response.contains("embedding"));
    }

    #[test]
    fn not_found_endpoint() {
        let pool = make_pool();
        let response = test_request(&pool, "GET /v1/unknown HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(response.contains("404"));
        assert!(response.contains("not found"));
    }

    #[test]
    fn bad_request_chat() {
        let pool = make_pool();
        let body = "isso não é json";
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );
        let response = test_request(&pool, &request);
        assert!(response.contains("400"));
    }

    #[test]
    fn import_hermes_skills_arquivo_unico() {
        let bb = temp_bb();
        let mut store = SkillStore::new(bb);
        let md = r#"---
name: rust-api
description: Build REST APIs in Rust
version: 1.0.0
platforms: linux, macos, windows
tags: backend, api, rust
---
## When to Use
Use this skill when building HTTP APIs.

## Quick Reference
- `cargo new my-api`

## Examples
```rust
fn hello() -> &'static str { "Hello" }
```
"#;
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), md).unwrap();

        let imported = import_hermes_skills(tmp.path().to_str().unwrap(), &mut store).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0], "rust-api");

        let skill = store.get("rust-api").unwrap();
        assert_eq!(skill.description, "Build REST APIs in Rust");
        assert!(skill.ast_signature.is_some());
    }

    #[test]
    fn import_hermes_skills_diretorio() {
        let bb = temp_bb();
        let mut store = SkillStore::new(bb);
        let dir = tempfile::tempdir().unwrap();

        let md1 = r#"---
name: skill-a
description: Skill A
version: 1.0.0
tags: a
---
## When to Use
Use A.
"#;
        let md2 = r#"---
name: skill-b
description: Skill B
version: 1.0.0
tags: b
---
## When to Use
Use B.
"#;
        std::fs::write(dir.path().join("a.md"), md1).unwrap();
        std::fs::write(dir.path().join("b.md"), md2).unwrap();

        let imported = import_hermes_skills(dir.path().to_str().unwrap(), &mut store).unwrap();
        assert_eq!(imported.len(), 2);
        assert!(imported.contains(&"skill-a".to_string()));
        assert!(imported.contains(&"skill-b".to_string()));
    }

    #[test]
    fn import_hermes_skills_arquivo_invalido() {
        let bb = temp_bb();
        let mut store = SkillStore::new(bb);
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "conteúdo inválido").unwrap();

        let result = import_hermes_skills(tmp.path().to_str().unwrap(), &mut store);
        assert!(result.is_err());
    }

    #[test]
    fn export_skills_vazia() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let json = export_skills(&store).unwrap();
        assert!(json.contains("\"data\": []"));
    }

    #[test]
    fn export_skills_com_dados() {
        let bb = temp_bb();
        let store = SkillStore::new(bb);
        let skill = Skill {
            name: "test-skill".into(),
            description: "Descrição de teste".into(),
            trigger_patterns: vec!["test".into()],
            ast_signature: None,
            file_target_pattern: None,
            instruction_template: "Instrução".into(),
            steps: vec![],
            templates: HashMap::new(),
            validation_cmds: vec![],
            last_used: 12345,
            usage_count: 0,
            success_rate: 0.0,
            created_from_dag_task_id: None,
            anti_conversation: true,
            idempotent: false,
            error_budget: 3,
            output_schema: None,
            allowed_tools: vec![],
            trust_level: SkillTrust::Untrusted,
            module_count: 1,
            mutation_history: vec![],
        };
        store.save(&skill).unwrap();

        let json = export_skills(&store).unwrap();
        assert!(json.contains("test-skill"));
        assert!(json.contains("arreio"));
    }

    #[test]
    fn import_extrai_ast_de_bloco_rust() {
        let bb = temp_bb();
        let mut store = SkillStore::new(bb);
        let md = r#"---
name: rust-ast-skill
description: Skill com AST
version: 1.0.0
tags: rust
---
## Examples
```rust
pub struct Foo {
    x: i32,
}

impl Foo {
    pub fn new(x: i32) -> Self {
        Self { x }
    }
}
```
"#;
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), md).unwrap();

        import_hermes_skills(tmp.path().to_str().unwrap(), &mut store).unwrap();
        let skill = store.get("rust-ast-skill").unwrap();
        assert!(skill.ast_signature.is_some());
        let sig = skill.ast_signature.unwrap();
        assert!(sig.contains("Foo"));
        assert!(sig.contains("new"));
    }
}
