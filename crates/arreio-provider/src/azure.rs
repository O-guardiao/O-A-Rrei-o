use crate::provider::{ChatRequest, ChatResponse, ProviderClient, ToolCall, ToolCallFunction};
use crate::rate_guard::RateLimitSnapshot;
use crate::tls_client::TlsClient;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

/// Provedor Azure OpenAI.
///
/// Usa a API de chat completions específica do Azure:
/// `POST /openai/deployments/{deployment}/chat/completions?api-version={version}`
///
/// A autenticação é feita via header `api-key` (não Authorization Bearer).
/// Comunicação síncrona sobre TLS nativo (`native-tls`).
#[derive(Clone)]
pub struct AzureProvider {
    endpoint: String,
    api_key: String,
    deployment: String,
    api_version: String,
    host: String,
    port: u16,
}

impl AzureProvider {
    pub fn new(endpoint: String, api_key: String, deployment: String) -> Self {
        let host = endpoint
            .replace("https://", "")
            .replace("http://", "")
            .trim_end_matches('/')
            .to_string();
        Self {
            endpoint,
            api_key,
            deployment,
            api_version: "2024-02-01".to_string(),
            host,
            port: 443,
        }
    }

    /// Monta a requisição HTTP/1.0 raw para chat completions.
    fn build_chat_request(&self, req: &ChatRequest) -> Result<String> {
        let messages = vec![
            serde_json::json!({"role": "system", "content": req.system}),
            serde_json::json!({"role": "user", "content": req.user}),
        ];

        let mut payload = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": false
        });

        if let Some(ref tools) = req.tools {
            payload["tools"] = serde_json::to_value(tools)?;
        }

        let body = serde_json::to_string(&payload)?;
        let path = format!(
            "/openai/deployments/{}/chat/completions?api-version={}",
            self.deployment, self.api_version
        );

        Ok(format!(
            "POST {} HTTP/1.0\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             api-key: {}\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            path,
            self.host,
            self.api_key,
            body.len(),
            body
        ))
    }

    /// Faz parsing da resposta HTTP bruta em JSON + rate-limit snapshot.
    fn parse_http_and_body(&self, raw: &str) -> Result<(Value, RateLimitSnapshot)> {
        let (status, headers, body) = crate::parse_http_response(raw)?;

        if status == 429 {
            let retry_after = headers
                .get("retry-after")
                .and_then(|v| v.parse::<u64>().ok());
            bail!("HTTP 429: rate limited, retry-after={:?}", retry_after);
        }
        if status < 200 || status >= 300 {
            bail!("HTTP {}: {}", status, body);
        }

        let rate_limit = RateLimitSnapshot::from_headers(&headers);
        let val: Value = serde_json::from_str(body.trim())
            .with_context(|| format!("JSON inválido na resposta Azure:\n{}", body))?;
        Ok((val, rate_limit))
    }

    /// Extrai `ChatResponse` a partir do JSON da API Azure (formato OpenAI-compatível).
    fn extract_chat_response(&self, val: &Value) -> Result<ChatResponse> {
        let message = val
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|choice| choice.get("message"))
            .context("campo 'choices[0].message' ausente na resposta Azure")?;

        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls = message
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let id = v
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let func = v.get("function")?;
                        let name = func.get("name").and_then(|n| n.as_str())?.to_string();
                        let args = func.get("arguments").and_then(|a| a.as_str())?.to_string();
                        Some(ToolCall {
                            id,
                            r#type: "function".to_string(),
                            function: ToolCallFunction {
                                name,
                                arguments: args,
                            },
                        })
                    })
                    .collect::<Vec<_>>()
            });

        let usage = val.get("usage");
        let tokens_in = usage
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tokens_out = usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        Ok(ChatResponse {
            content,
            tool_calls: if tool_calls.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
                None
            } else {
                tool_calls
            },
            tokens_in,
            tokens_out,
            rate_limit: None,
            reasoning_content: None,
        })
    }

    /// Envia requisição TLS e retorna resposta raw.
    fn tls_post(&self, request: &str) -> Result<String> {
        let mut stream = TlsClient::connect(&self.host, self.port)
            .with_context(|| format!("falha de conexão TLS para {}", self.host))?;
        stream
            .write_all(request.as_bytes())
            .context("falha ao enviar requisição TLS")?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .context("falha ao ler resposta TLS")?;
        Ok(response)
    }
}

impl ProviderClient for AzureProvider {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let request_str = self.build_chat_request(&req)?;
        let delays = [1u64, 2, 4];
        let mut last_err = anyhow::anyhow!("sem tentativas");

        for (i, &delay) in delays.iter().enumerate() {
            match self.tls_post(&request_str) {
                Ok(raw) => {
                    let (val, rate_limit) = self.parse_http_and_body(&raw)?;
                    let mut resp = self.extract_chat_response(&val)?;
                    resp.rate_limit = Some(rate_limit);
                    return Ok(resp);
                }
                Err(e) => {
                    last_err = e;
                    if i < delays.len() - 1 {
                        eprintln!(
                            "[azure] retry {}/{} para {}: {}",
                            i + 1,
                            delays.len(),
                            self.endpoint,
                            last_err
                        );
                        thread::sleep(Duration::from_secs(delay));
                    }
                }
            }
        }

        bail!(
            "Azure falhou após {} tentativas: {}",
            delays.len(),
            last_err
        )
    }
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        let messages = vec![
            serde_json::json!({"role": "system", "content": req.system}),
            serde_json::json!({"role": "user", "content": req.user}),
        ];

        let mut payload = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": true
        });

        if let Some(ref tools) = req.tools {
            payload["tools"] = serde_json::to_value(tools)?;
        }

        let body = serde_json::to_string(&payload)?;
        let path = format!(
            "/openai/deployments/{}/chat/completions?api-version={}",
            self.deployment, self.api_version
        );
        let headers: Vec<(&str, &str)> = vec![
            ("Content-Type", "application/json"),
            ("api-key", &self.api_key),
        ];

        let request = crate::tls_client::build_post_request(
            &self.host,
            &path,
            &headers,
            &body,
        );

        let mut stream = TlsClient::connect(&self.host, self.port)?;
        stream.write_all(request.as_bytes())?;

        // Azure usa mesmo formato SSE da OpenAI
        let chunks = read_sse_lines_azure(&mut stream)?;
        let texts: Vec<Result<String>> = chunks.into_iter().map(|c| Ok(c)).collect();
        Ok(Box::new(texts.into_iter()))
    }

    fn name(&self) -> &'static str {
        "azure"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        Box::new(self.clone())
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        let input_cost = input_tokens as f64 * 30.0 / 1_000_000.0;
        let output_cost = output_tokens as f64 * 60.0 / 1_000_000.0;
        input_cost + output_cost
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let payload = serde_json::json!({
            "input": texts,
            "model": self.deployment,
        });
        let body = serde_json::to_string(&payload)?;
        let path = format!(
            "/openai/deployments/{}/embeddings?api-version={}",
            self.deployment, self.api_version
        );
        let request = format!(
            "POST {} HTTP/1.0\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             api-key: {}\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            path, self.host, self.api_key, body.len(), body
        );

        let delays = [1u64, 2, 4];
        let mut last_err = anyhow::anyhow!("sem tentativas");
        for (i, &delay) in delays.iter().enumerate() {
            match self.tls_post(&request) {
                Ok(raw) => {
                    let (_, _, json_body) = crate::parse_http_response(&raw)?;
                    let val: Value = serde_json::from_str(json_body.trim())
                        .with_context(|| format!("JSON inválido na resposta Azure embeddings: {}", json_body))?;
                    let data = val
                        .get("data")
                        .and_then(|d| d.as_array())
                        .context("resposta de embeddings sem campo 'data'")?;
                    let mut embeddings = Vec::new();
                    for item in data {
                        let embedding = item
                            .get("embedding")
                            .and_then(|e| e.as_array())
                            .context("item de embedding sem campo 'embedding'")?;
                        let vec: Vec<f32> = embedding
                            .iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect();
                        embeddings.push(vec);
                    }
                    return Ok(embeddings);
                }
                Err(e) => {
                    last_err = e;
                    if i < delays.len() - 1 {
                        eprintln!("[azure] embed retry {}/{}: {}", i + 1, delays.len(), last_err);
                        thread::sleep(Duration::from_secs(delay));
                    }
                }
            }
        }
        bail!("Azure embed falhou após {} tentativas: {}", delays.len(), last_err)
    }
}

// ── SSE Streaming helpers ────────────────────────────────────────────────────

/// Lê chunks SSE do Azure OpenAI (formato OpenAI-compatible: data: JSON)
fn read_sse_lines_azure(reader: &mut dyn std::io::Read) -> Result<Vec<String>> {
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
                if let Some(text) = json
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                {
                    texts.push(text.to_string());
                }
            }
        }
    }
    Ok(texts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatRequest, ToolDescriptor, ToolFunction};

    fn provider() -> AzureProvider {
        AzureProvider::new(
            "https://meu-recurso.openai.azure.com".to_string(),
            "fake-api-key".to_string(),
            "gpt-4-deployment".to_string(),
        )
    }

    fn fake_response(content: &str, prompt_tokens: u64, completion_tokens: u64) -> String {
        format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": content
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": prompt_tokens + completion_tokens
                }
            })
        )
    }

    #[test]
    fn new_parseia_host_https() {
        let p = provider();
        assert_eq!(p.host, "meu-recurso.openai.azure.com");
        assert_eq!(p.port, 443);
    }

    #[test]
    fn new_parseia_host_http() {
        let p = AzureProvider::new(
            "http://outro-recurso.openai.azure.com/".to_string(),
            "k".to_string(),
            "d".to_string(),
        );
        assert_eq!(p.host, "outro-recurso.openai.azure.com");
    }

    #[test]
    fn name_retorna_azure() {
        assert_eq!(provider().name(), "azure");
    }

    #[test]
    fn clone_box_cria_independente() {
        let p = provider();
        let cloned = p.clone_box();
        assert_eq!(cloned.name(), "azure");
    }

    #[test]
    fn cost_estimate_zero_tokens() {
        let p = provider();
        assert_eq!(p.cost_estimate(0, 0), 0.0);
    }

    #[test]
    fn cost_estimate_milhao_tokens() {
        let p = provider();
        // 1M input = $30, 1M output = $60
        let cost = p.cost_estimate(1_000_000, 1_000_000);
        assert!((cost - 90.0).abs() < f64::EPSILON);
    }

    #[test]
    fn embed_parseia_resposta_azure() {
        // Simula resposta real da API Azure embeddings
        let raw = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
            serde_json::json!({
                "data": [
                    {"embedding": [0.1, 0.2, 0.3]},
                    {"embedding": [0.4, 0.5, 0.6]}
                ]
            })
        );
        let (_, _, json_body) = crate::parse_http_response(&raw).unwrap();
        let val: Value = serde_json::from_str(json_body.trim()).unwrap();
        let data = val.get("data").and_then(|d| d.as_array()).unwrap();
        assert_eq!(data.len(), 2);
        let emb0: Vec<f32> = data[0]
            .get("embedding")
            .and_then(|e| e.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();
        assert_eq!(emb0.len(), 3);
        assert!((emb0[0] - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_http_200_extrai_resposta() {
        let p = provider();
        let raw = fake_response("Olá do Azure", 10, 5);
        let (val, _rl) = p.parse_http_and_body(&raw).unwrap();

        let content = val["choices"][0]["message"]["content"].as_str().unwrap();
        assert_eq!(content, "Olá do Azure");
    }

    #[test]
    fn extract_chat_response_completo() {
        let p = provider();
        let raw = fake_response("Resposta completa", 100, 50);
        let (val, _) = p.parse_http_and_body(&raw).unwrap();
        let resp = p.extract_chat_response(&val).unwrap();

        assert_eq!(resp.content, "Resposta completa");
        assert_eq!(resp.tokens_in, 100);
        assert_eq!(resp.tokens_out, 50);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn extract_chat_response_com_tool_calls() {
        let p = provider();
        let raw = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "tool_calls": [{
                            "id": "call_123",
                            "type": "function",
                            "function": {
                                "name": "get_status",
                                "arguments": "{\"id\":1}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 10,
                    "total_tokens": 30
                }
            })
        );
        let (val, _) = p.parse_http_and_body(&raw).unwrap();
        let resp = p.extract_chat_response(&val).unwrap();

        assert_eq!(resp.content, "");
        let tc = resp.tool_calls.unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "get_status");
        assert_eq!(tc[0].function.arguments, "{\"id\":1}");
    }

    #[test]
    fn parse_http_429_retorna_erro() {
        let p = provider();
        let raw = "HTTP/1.0 429 Too Many Requests\r\nretry-after: 120\r\n\r\n{}";
        let err = p.parse_http_and_body(raw).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("429"), "esperado erro 429, got: {}", msg);
    }

    #[test]
    fn parse_http_500_retorna_erro() {
        let p = provider();
        let raw = "HTTP/1.0 500 Internal Server Error\r\n\r\n{\"error\":\"boom\"}";
        let err = p.parse_http_and_body(raw).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("500"), "esperado erro 500, got: {}", msg);
    }

    #[test]
    fn build_request_contem_api_key_header() {
        let p = provider();
        let req = ChatRequest {
            messages: Vec::new(),
            model: "gpt-4".to_string(),
            system: "sys".to_string(),
            user: "user".to_string(),
            tools: None,
        };
        let raw = p.build_chat_request(&req).unwrap();
        assert!(raw.contains("api-key: fake-api-key"));
    }

    #[test]
    fn build_request_contem_deployment_e_api_version() {
        let p = provider();
        let req = ChatRequest {
            messages: Vec::new(),
            model: "gpt-4".to_string(),
            system: "s".to_string(),
            user: "u".to_string(),
            tools: None,
        };
        let raw = p.build_chat_request(&req).unwrap();
        assert!(raw.contains(
            "/openai/deployments/gpt-4-deployment/chat/completions?api-version=2024-02-01"
        ));
    }

    #[test]
    fn build_request_inclui_tools() {
        let p = provider();
        let req = ChatRequest {
            messages: Vec::new(),
            model: "gpt-4".to_string(),
            system: "s".to_string(),
            user: "u".to_string(),
            tools: Some(vec![ToolDescriptor {
                r#type: "function".to_string(),
                function: ToolFunction {
                    name: "fn1".to_string(),
                    description: "desc".to_string(),
                    parameters: serde_json::json!({}),
                },
            }]),
        };
        let raw = p.build_chat_request(&req).unwrap();
        assert!(raw.contains("\"tools\""));
        assert!(raw.contains("fn1"));
    }

    #[test]
    fn extract_chat_response_falha_sem_choices() {
        let p = provider();
        let raw = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{}",
            serde_json::json!({"usage": {"prompt_tokens": 1, "completion_tokens": 1}})
        );
        let (val, _) = p.parse_http_and_body(&raw).unwrap();
        let err = p.extract_chat_response(&val).unwrap_err();
        assert!(err.to_string().contains("choices"));
    }
}
