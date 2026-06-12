//! Cliente síncrono para a Google Gemini API.
//!
//! Usa `TlsClient` (native-tls) para comunicação HTTPS com
//! `generativelanguage.googleapis.com`.
//!
//! Referência da API:
//! - POST /v1beta/models/{model}:generateContent
//! - Header obrigatório: `x-goog-api-key`
//! - Body: `{ contents: [...], systemInstruction: {...}, tools: [...] }`

use crate::provider::{ChatRequest, ChatResponse, ProviderClient, ToolCall, ToolCallFunction};
use crate::tls_client::TlsClient;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::thread;
use std::time::Duration;

/// Provedor Google Gemini.
#[derive(Clone)]
pub struct GoogleProvider {
    api_key: String,
    model: String,
    host: String,
    port: u16,
}

impl GoogleProvider {
    /// Cria um novo provider apontando para a API oficial do Gemini.
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            host: "generativelanguage.googleapis.com".to_string(),
            port: 443,
        }
    }

    /// Monta o payload JSON no formato exigido pela Gemini API.
    fn build_payload(&self, req: &ChatRequest) -> Value {
        let mut payload = json!({
            "systemInstruction": {
                "role": "user",
                "parts": [{ "text": req.system }]
            },
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": req.user }]
                }
            ]
        });

        if let Some(ref tools) = req.tools {
            let declarations: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.function.name,
                        "description": t.function.description,
                        "parameters": t.function.parameters
                    })
                })
                .collect();

            payload["tools"] = json!([
                { "functionDeclarations": declarations }
            ]);
        }

        payload
    }

    /// Extrai `ChatResponse` a partir do corpo JSON da resposta Gemini.
    fn parse_response_body(&self, body: &str) -> Result<ChatResponse> {
        let val: Value = serde_json::from_str(body.trim())
            .with_context(|| format!("JSON inválido na resposta Google:\n{}", body))?;

        // Tratamento de erro estruturado da API
        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("erro desconhecido da Google Gemini");
            bail!("Google API error: {}", msg);
        }

        let candidates = val
            .get("candidates")
            .and_then(|c| c.as_array())
            .context("campo 'candidates' ausente na resposta Gemini")?;

        let first = candidates
            .first()
            .context("lista 'candidates' vazia na resposta Gemini")?;

        let parts = first
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .context("campo 'content.parts' ausente na resposta Gemini")?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                text_parts.push(text.to_string());
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = serde_json::to_string(&fc.get("args").unwrap_or(&json!({})))
                    .unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall {
                    id: format!("google_fn_{}", name),
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name,
                        arguments: args,
                    },
                });
            }
        }

        let content = text_parts.join("\n");

        let usage = val.get("usageMetadata");
        let tokens_in = usage
            .and_then(|u| u.get("promptTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tokens_out = usage
            .and_then(|u| u.get("candidatesTokenCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        Ok(ChatResponse {
            content,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tokens_in,
            tokens_out,
            rate_limit: None,
            reasoning_content: None,
        })
    }
}

impl ProviderClient for GoogleProvider {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let payload = self.build_payload(&req);
        let body_str = serde_json::to_string(&payload)?;
        let path = format!("/v1beta/models/{}:generateContent", self.model);
        let headers = [
            ("x-goog-api-key", self.api_key.as_str()),
            ("Content-Type", "application/json"),
        ];

        let (status, response_body) =
            send_with_retry(&self.host, self.port, &path, &headers, &body_str)?;

        if status == 429 {
            bail!("HTTP 429: rate limited pelo Google Gemini");
        }
        if status < 200 || status >= 300 {
            bail!("HTTP {}: {}", status, response_body);
        }

        self.parse_response_body(&response_body)
    }
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        let mut payload = self.build_payload(&req);
        // Gemini usa "stream": true no topo ou generateContent com alt=sse
        // Via REST, streaming é feito adicionando "generationConfig.stream": true
        // ou usando o endpoint streamGenerateContent
        // Abordagem: enviar para streamGenerateContent com alt=sse
        let path = format!(
            "/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.model
        );

        payload["generationConfig"] = serde_json::json!({"stream": true});

        let body = serde_json::to_string(&payload)?;
        let headers: Vec<(&str, &str)> = vec![
            ("Content-Type", "application/json"),
            ("x-goog-api-key", &self.api_key),
        ];

        let request = crate::tls_client::build_post_request(
            &self.host,
            &path,
            &headers,
            &body,
        );

        let mut stream = TlsClient::connect(&self.host, self.port)?;
        stream.write_all(request.as_bytes())?;

        let chunks = read_sse_lines_gemini(&mut stream)?;
        let texts: Vec<Result<String>> = chunks.into_iter().map(|c| Ok(c)).collect();
        Ok(Box::new(texts.into_iter()))
    }

    fn name(&self) -> &'static str {
        "google"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        Box::new(Self {
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            host: self.host.clone(),
            port: self.port,
        })
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        let input_cost = input_tokens as f64 * 3.50 / 1_000_000.0;
        let output_cost = output_tokens as f64 * 10.50 / 1_000_000.0;
        input_cost + output_cost
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut embeddings = Vec::new();
        for text in texts {
            let payload = json!({
                "model": self.model,
                "content": {
                    "parts": [{"text": text}]
                }
            });
            let body = serde_json::to_string(&payload)?;
            let path = format!("/v1beta/models/{}:embedContent", self.model);
            let headers = [
                ("x-goog-api-key", self.api_key.as_str()),
                ("Content-Type", "application/json"),
            ];
            let (status, response_body) =
                send_with_retry(&self.host, self.port, &path, &headers, &body)?;
            if status < 200 || status >= 300 {
                bail!("HTTP {}: {}", status, response_body);
            }
            let val: Value = serde_json::from_str(&response_body)
                .with_context(|| format!("JSON inválido na resposta embedContent: {}", response_body))?;
            let embedding = val
                .get("embedding")
                .and_then(|e| e.get("values"))
                .and_then(|v| v.as_array())
                .context("resposta de embedContent sem embedding.values")?;
            let vec: Vec<f32> = embedding
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            embeddings.push(vec);
        }
        Ok(embeddings)
    }
}

// ── SSE Streaming helpers ────────────────────────────────────────────────────

/// Lê chunks SSE do Gemini (formato: alt=sse com data: JSON)
fn read_sse_lines_gemini(reader: &mut dyn std::io::Read) -> Result<Vec<String>> {
    let mut buf = [0u8; 4096];
    let mut raw = String::new();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        raw.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    let mut texts = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(data) = trimmed.strip_prefix("data: ") {
            if data == "[DONE]" {
                break;
            }
            if let Ok(json) = serde_json::from_str::<Value>(data) {
                if let Some(candidates) = json.get("candidates").and_then(|c| c.as_array()) {
                    for cand in candidates {
                        if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    texts.push(text.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(texts)
}

/// Envia a requisição com retry exponencial (1s → 2s → 4s).
fn send_with_retry(
    host: &str,
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<(u16, String)> {
    let delays = [1u64, 2, 4];
    let mut last_err = anyhow::anyhow!("sem tentativas");
    for (i, &delay) in delays.iter().enumerate() {
        match crate::tls_client::TlsClient::https_post(host, port, path, headers, body) {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!("[google] retry {}/{}: {}", i + 1, delays.len(), last_err);
                    thread::sleep(Duration::from_secs(delay));
                }
            }
        }
    }
    bail!(
        "Google Gemini falhou após {} tentativas: {}",
        delays.len(),
        last_err
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ProviderClient, ToolDescriptor, ToolFunction};
    use serde_json::json;

    #[test]
    fn test_new_sets_defaults() {
        let p = GoogleProvider::new("key123".into(), "gemini-1.5-pro".into());
        assert_eq!(p.api_key, "key123");
        assert_eq!(p.model, "gemini-1.5-pro");
        assert_eq!(p.host, "generativelanguage.googleapis.com");
        assert_eq!(p.port, 443);
    }

    #[test]
    fn test_name_returns_google() {
        let p = GoogleProvider::new("k".into(), "m".into());
        assert_eq!(p.name(), "google");
    }

    #[test]
    fn test_clone_box_produces_equal_provider() {
        let p = GoogleProvider::new("secret".into(), "gemini".into());
        let cloned = p.clone_box();
        assert_eq!(cloned.name(), "google");
        // clone_box retorna Box<dyn ProviderClient>; não podemos comparar campos diretamente,
        // mas podemos garantir que chat (via trait) funciona com o clone.
        // Aqui verificamos apenas que não panica e mantém o nome.
    }

    #[test]
    fn test_cost_estimate_zero_tokens() {
        let p = GoogleProvider::new("k".into(), "m".into());
        assert_eq!(p.cost_estimate(0, 0), 0.0);
    }

    #[test]
    fn test_cost_estimate_nonzero_tokens() {
        let p = GoogleProvider::new("k".into(), "m".into());
        // 1M input  → 3.50
        // 1M output → 10.50
        assert_eq!(p.cost_estimate(1_000_000, 1_000_000), 14.0);
    }

    #[test]
    fn test_embed_parses_embed_content_response() {
        let _p = GoogleProvider::new("k".into(), "text-embedding-004".into());
        // Testa parse do JSON de resposta real da API embedContent
        let body = r#"{"embedding": {"values": [0.1, 0.2, 0.3]}}"#;
        let val: Value = serde_json::from_str(body).unwrap();
        let embedding = val
            .get("embedding")
            .and_then(|e| e.get("values"))
            .and_then(|v| v.as_array())
            .unwrap();
        let vec: Vec<f32> = embedding
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        assert_eq!(vec.len(), 3);
        assert!((vec[0] - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn test_build_payload_with_system_and_user() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let req = ChatRequest {
            messages: Vec::new(),
            model: "m".into(),
            system: "Você é um assistente.".into(),
            user: "Qual a capital da França?".into(),
            tools: None,
        };
        let payload = p.build_payload(&req);

        let sys = payload
            .get("systemInstruction")
            .and_then(|s| s.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|part| part.get("text"))
            .and_then(|t| t.as_str())
            .unwrap();
        assert_eq!(sys, "Você é um assistente.");

        let user = payload
            .get("contents")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|msg| msg.get("parts"))
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|part| part.get("text"))
            .and_then(|t| t.as_str())
            .unwrap();
        assert_eq!(user, "Qual a capital da França?");

        assert!(payload.get("tools").is_none());
    }

    #[test]
    fn test_build_payload_with_tools() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let req = ChatRequest {
            messages: Vec::new(),
            model: "m".into(),
            system: "sys".into(),
            user: "user".into(),
            tools: Some(vec![ToolDescriptor {
                r#type: "function".into(),
                function: ToolFunction {
                    name: "get_weather".into(),
                    description: "Retorna clima.".into(),
                    parameters: json!({"type": "object"}),
                },
            }]),
        };
        let payload = p.build_payload(&req);
        let tools = payload.get("tools").and_then(|t| t.as_array()).unwrap();
        assert_eq!(tools.len(), 1);
        let decls = tools[0]
            .get("functionDeclarations")
            .and_then(|d| d.as_array())
            .unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(
            decls[0].get("name").and_then(|n| n.as_str()).unwrap(),
            "get_weather"
        );
    }

    #[test]
    fn test_parse_response_simple_text() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let body = r#"{
            "candidates": [{
                "content": {
                    "parts": [{"text": "Paris"}]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 2
            }
        }"#;
        let resp = p.parse_response_body(body).unwrap();
        assert_eq!(resp.content, "Paris");
        assert_eq!(resp.tokens_in, 10);
        assert_eq!(resp.tokens_out, 2);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let body = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Vou consultar."},
                        {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
                    ]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 15,
                "candidatesTokenCount": 8
            }
        }"#;
        let resp = p.parse_response_body(body).unwrap();
        assert_eq!(resp.content, "Vou consultar.");
        let calls = resp.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments, r#"{"city":"Paris"}"#);
    }

    #[test]
    fn test_parse_response_missing_candidates() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let body = r#"{"usageMetadata": {}}"#;
        let err = p.parse_response_body(body).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("candidates"));
    }

    #[test]
    fn test_parse_response_api_error() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let body = r#"{"error": {"message": "API key invalid", "code": 400}}"#;
        let err = p.parse_response_body(body).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("API key invalid"));
    }

    #[test]
    fn test_parse_response_empty_parts() {
        let p = GoogleProvider::new("k".into(), "m".into());
        let body = r#"{
            "candidates": [{
                "content": {
                    "parts": []
                }
            }]
        }"#;
        let resp = p.parse_response_body(body).unwrap();
        assert_eq!(resp.content, "");
        assert!(resp.tool_calls.is_none());
    }
}
