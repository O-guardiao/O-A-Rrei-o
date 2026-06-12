use crate::provider::{ChatRequest, ChatResponse, ProviderClient};
use crate::rate_guard::RateLimitSnapshot;
use crate::tls_client::TlsClient;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

/// Provedor Anthropic Messages API (/v1/messages).
/// Suporta tanto TCP puro (MVP/proxy local) quanto TLS real via TlsClient.
#[derive(Clone)]
pub struct AnthropicProvider {
    host: String,
    port: u16,
    api_key: String,
    use_tls: bool,
}

impl AnthropicProvider {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        api_key: impl Into<String>,
        use_tls: bool,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            api_key: api_key.into(),
            use_tls,
        }
    }
}

impl ProviderClient for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        Box::new(self.clone())
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        // Preços reais do Claude 3.5 Sonnet: $3.00 / 1M input tokens, $15.00 / 1M output tokens.
        let input_cost = (input_tokens as f64) * 3.00 / 1_000_000.0;
        let output_cost = (output_tokens as f64) * 15.00 / 1_000_000.0;
        input_cost + output_cost
    }

    fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        Err(anyhow::anyhow!("embed não implementado para Anthropic"))
    }

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let mut payload = serde_json::json!({
            "model": req.model,
            "max_tokens": 4096,
            "system": req.system,
            "messages": [
                {"role": "user", "content": req.user}
            ]
        });

        // Anthropic usa formato "tools" no topo
        if let Some(ref tools) = req.tools {
            let anthropic_tools: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.function.name,
                        "description": t.function.description,
                        "input_schema": t.function.parameters
                    })
                })
                .collect();
            payload["tools"] = serde_json::json!(anthropic_tools);
        }

        let (response, rate_limit) =
            send_with_retry(&self.host, self.port, &self.api_key, &payload, self.use_tls)?;

        // Anthropic retorna content como array de blocos (text ou tool_use)
        let content_blocks = response
            .get("content")
            .and_then(|c| c.as_array())
            .context("campo 'content' ausente na resposta Anthropic")?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in content_blocks {
            if let Some(t) = block.get("type").and_then(|t| t.as_str()) {
                match t {
                    "text" => {
                        if let Some(txt) = block.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(txt.to_string());
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = serde_json::to_string(
                            &block.get("input").unwrap_or(&serde_json::json!({})),
                        )
                        .unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(crate::provider::ToolCall {
                            id,
                            r#type: "function".to_string(),
                            function: crate::provider::ToolCallFunction {
                                name,
                                arguments: args,
                            },
                        });
                    }
                    _ => {}
                }
            }
        }

        let content = text_parts.join("\n");

        let usage = response.get("usage");
        let tokens_in = usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tokens_out = usage
            .and_then(|u| u.get("output_tokens"))
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
            rate_limit: Some(rate_limit),
            reasoning_content: None,
        })
    }
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        let mut payload = serde_json::json!({
            "model": req.model,
            "max_tokens": 4096,
            "system": req.system,
            "messages": [
                {"role": "user", "content": req.user}
            ],
            "stream": true
        });

        if let Some(ref tools) = req.tools {
            let anthropic_tools: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.function.name,
                        "description": t.function.description,
                        "input_schema": t.function.parameters
                    })
                })
                .collect();
            payload["tools"] = serde_json::json!(anthropic_tools);
        }

        let body = serde_json::to_string(&payload)?;
        let headers: Vec<(&str, &str)> = vec![
            ("Content-Type", "application/json"),
            ("x-api-key", &self.api_key),
            ("anthropic-version", "2023-06-01"),
        ];

        let request = crate::tls_client::build_post_request(
            &self.host,
            "/v1/messages",
            &headers,
            &body,
        );

        let mut stream = if self.use_tls {
            TlsClient::connect(&self.host, 443)?
        } else {
            return Err(anyhow::anyhow!("Anthropic requer TLS"));
        };
        stream.write_all(request.as_bytes())?;

        let chunks = read_sse_lines_anthropic(&mut stream)?;
        let texts: Vec<Result<String>> = chunks.into_iter().map(|c| Ok(c)).collect();
        Ok(Box::new(texts.into_iter()))
    }
}

// ── SSE Streaming helpers ────────────────────────────────────────────────────

/// Lê chunks SSE do Anthropic (formato: event: / data:)
fn read_sse_lines_anthropic(reader: &mut dyn std::io::Read) -> Result<Vec<String>> {
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
        if let Some(data) = line.strip_prefix("data: ") {
            let chunk = serde_json::from_str::<Value>(data);
            if let Ok(json) = chunk {
                // Anthropic SSE: delta type
                if let Some(t) = json.get("type").and_then(|v| v.as_str()) {
                    match t {
                        "content_block_delta" => {
                            if let Some(text) = json.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                                texts.push(text.to_string());
                            }
                        }
                        "message_stop" => break,
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(texts)
}

fn send_with_retry(
    host: &str,
    port: u16,
    api_key: &str,
    payload: &Value,
    use_tls: bool,
) -> Result<(Value, RateLimitSnapshot)> {
    let delays = [1u64, 2, 4];
    let mut last_err = anyhow::anyhow!("sem tentativas");
    for (i, &delay) in delays.iter().enumerate() {
        let result = if use_tls {
            tls_post(host, api_key, payload)
        } else {
            tcp_post(host, port, api_key, payload)
        };
        match result {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!("[anthropic] retry {}/{}: {}", i + 1, delays.len(), last_err);
                    thread::sleep(Duration::from_secs(delay));
                }
            }
        }
    }
    bail!(
        "Anthropic falhou após {} tentativas: {}",
        delays.len(),
        last_err
    )
}

fn tcp_post(
    host: &str,
    port: u16,
    api_key: &str,
    payload: &Value,
) -> Result<(Value, RateLimitSnapshot)> {
    let body = serde_json::to_string(payload)?;
    let request = format!(
        "POST /v1/messages HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         x-api-key: {}\r\n\
         anthropic-version: 2023-06-01\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        host,
        port,
        api_key,
        body.len(),
        body
    );

    let mut stream = TcpStream::connect((host, port))
        .with_context(|| format!("não foi possível conectar a {}:{}", host, port))?;

    stream
        .write_all(request.as_bytes())
        .context("falha ao enviar request HTTP")?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .context("falha ao ler resposta HTTP")?;

    let (status, headers, json_body) = crate::parse_http_response(&raw)?;
    parse_anthropic_response(status, headers, &json_body)
}

fn tls_post(host: &str, api_key: &str, payload: &Value) -> Result<(Value, RateLimitSnapshot)> {
    let body = serde_json::to_string(payload)?;
    let headers = [
        ("x-api-key", api_key),
        ("anthropic-version", "2023-06-01"),
        ("Content-Type", "application/json"),
    ];
    let (status, json_body) = TlsClient::https_post(host, 443, "/v1/messages", &headers, &body)
        .with_context(|| format!("falha na requisição HTTPS para {}", host))?;
    parse_anthropic_response(status, HashMap::new(), &json_body)
}

/// Parseia a resposta HTTP da Anthropic, validando status, extraindo rate limits
/// e convertendo o corpo JSON em Value.
fn parse_anthropic_response(
    status: u16,
    headers: HashMap<String, String>,
    json_body: &str,
) -> Result<(Value, RateLimitSnapshot)> {
    if status == 429 {
        let retry_after = headers
            .get("retry-after")
            .and_then(|v| v.parse::<u64>().ok());
        bail!("HTTP 429: rate limited, retry-after={:?}", retry_after);
    }
    if status < 200 || status >= 300 {
        bail!("HTTP {}: {}", status, json_body);
    }

    let rate_limit = RateLimitSnapshot::from_headers(&headers);

    // Tratamento de erro Anthropic (campo "error")
    if let Ok(val) = serde_json::from_str::<Value>(json_body.trim()) {
        if let Some(err) = val.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("erro desconhecido da Anthropic");
            bail!("Anthropic API error: {}", msg);
        }
        return Ok((val, rate_limit));
    }

    bail!("JSON inválido na resposta Anthropic:\n{}", json_body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_retorna_anthropic() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        assert_eq!(p.name(), "anthropic");
    }

    #[test]
    fn clone_box_retorna_anthropic() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        let cloned = p.clone_box();
        assert_eq!(cloned.name(), "anthropic");
    }

    #[test]
    fn new_sem_tls() {
        let p = AnthropicProvider::new("api.anthropic.com", 8080, "sk-test", false);
        assert_eq!(p.host, "api.anthropic.com");
        assert_eq!(p.port, 8080);
        assert_eq!(p.api_key, "sk-test");
        assert!(!p.use_tls);
    }

    #[test]
    fn new_com_tls() {
        let p = AnthropicProvider::new("api.anthropic.com", 443, "sk-test", true);
        assert_eq!(p.host, "api.anthropic.com");
        assert_eq!(p.port, 443);
        assert_eq!(p.api_key, "sk-test");
        assert!(p.use_tls);
    }

    #[test]
    fn clone_preserva_tls_flag() {
        let p = AnthropicProvider::new("api.anthropic.com", 443, "sk-test", true);
        let cloned = p.clone();
        assert!(cloned.use_tls);
        assert_eq!(cloned.host, "api.anthropic.com");
    }

    #[test]
    fn cost_estimate_claude_35_sonnet() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        // 1M input + 1M output = $3.00 + $15.00 = $18.00
        let cost = p.cost_estimate(1_000_000, 1_000_000);
        assert!(
            (cost - 18.0).abs() < 0.0001,
            "custo deve ser 18.00 USD, foi {}",
            cost
        );
    }

    #[test]
    fn cost_estimate_zero_tokens() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        let cost = p.cost_estimate(0, 0);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn cost_estimate_apenas_input() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        // 500k input tokens = $1.50
        let cost = p.cost_estimate(500_000, 0);
        assert!(
            (cost - 1.5).abs() < 0.0001,
            "custo deve ser 1.50 USD, foi {}",
            cost
        );
    }

    #[test]
    fn cost_estimate_apenas_output() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        // 200k output tokens = $3.00
        let cost = p.cost_estimate(0, 200_000);
        assert!(
            (cost - 3.0).abs() < 0.0001,
            "custo deve ser 3.00 USD, foi {}",
            cost
        );
    }

    #[test]
    fn embed_retorna_erro() {
        let p = AnthropicProvider::new("localhost", 8080, "key", false);
        let result = p.embed(vec!["texto".to_string()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("embed não implementado"));
    }

    #[test]
    fn parse_anthropic_response_sucesso() {
        let headers = HashMap::new();
        let body = r#"{"id":"msg_01","content":[{"type":"text","text":"ola"}],"usage":{"input_tokens":10,"output_tokens":5}}"#;
        let (val, rl) = parse_anthropic_response(200, headers, body).unwrap();
        assert_eq!(val["id"], "msg_01");
        assert!(rl.remaining_requests.is_none()); // sem headers de rate limit
    }

    #[test]
    fn parse_anthropic_response_429() {
        let mut headers = HashMap::new();
        headers.insert("retry-after".to_string(), "60".to_string());
        let result = parse_anthropic_response(429, headers, "{}");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("429"));
        assert!(err.contains("60"));
    }

    #[test]
    fn parse_anthropic_response_http_500() {
        let headers = HashMap::new();
        let result = parse_anthropic_response(500, headers, "Internal Server Error");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("500"));
    }

    #[test]
    fn parse_anthropic_response_json_invalido() {
        let headers = HashMap::new();
        let result = parse_anthropic_response(200, headers, "nao-eh-json");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("JSON inválido"));
    }

    #[test]
    fn parse_anthropic_response_api_error() {
        let headers = HashMap::new();
        let body = r#"{"error":{"type":"invalid_request_error","message":"max_tokens inválido"}}"#;
        let result = parse_anthropic_response(200, headers, body);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_tokens inválido"));
    }

    #[test]
    fn parse_anthropic_response_rate_limit_headers() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-ratelimit-remaining-requests".to_string(),
            "99".to_string(),
        );
        headers.insert(
            "x-ratelimit-reset-requests".to_string(),
            "1234567890".to_string(),
        );
        let body = r#"{"id":"msg","content":[],"usage":{"input_tokens":1,"output_tokens":1}}"#;
        let (_, rl) = parse_anthropic_response(200, headers, body).unwrap();
        assert_eq!(rl.remaining_requests, Some(99));
        assert_eq!(rl.reset_timestamp, Some(1234567890));
    }

    #[test]
    fn tcp_post_host_invalido_falha() {
        let payload = serde_json::json!({"model":"claude"});
        let result = tcp_post("127.0.0.1", 1, "key", &payload);
        assert!(result.is_err());
    }

    #[test]
    fn tls_post_host_invalido_falha() {
        let payload = serde_json::json!({"model":"claude"});
        let result = tls_post("127.0.0.1", "key", &payload);
        assert!(result.is_err());
    }
}
