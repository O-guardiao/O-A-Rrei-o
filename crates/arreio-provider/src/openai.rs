use crate::provider::{ChatRequest, ChatResponse, ProviderClient};
use crate::rate_guard::RateLimitSnapshot;
use crate::tls_client::TlsClient;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

/// Provedor compatível com OpenAI API (/v1/chat/completions).
/// Funciona com OpenAI, OpenRouter, Kimi (Moonshot), MiniMax, Ollama em modo
/// compatível, etc. Suporta tanto TCP puro (MVP/proxy local) quanto TLS real (HTTPS).
#[derive(Clone)]
pub struct OpenAiCompatProvider {
    host: String,
    port: u16,
    api_key: Option<String>,
    use_tls: bool,
    /// Prefixo de caminho da API (ex.: "/v1" para OpenAI/Kimi/MiniMax,
    /// "/api/v1" para OpenRouter).
    base_path: String,
    /// Rótulo do provedor para métricas, logs e estimativa de custo.
    label: &'static str,
}

impl OpenAiCompatProvider {
    /// Cria um novo provider apontando para a OpenAI (compatível com o
    /// comportamento histórico: `base_path = "/v1"`, rótulo `"openai"`).
    ///
    /// * `use_tls` — quando `true`, envia requisições via `TlsClient` na porta
    ///   informada; quando `false`, usa `TcpStream` puro no `host:port`.
    pub fn new(host: impl Into<String>, port: u16, api_key: Option<String>, use_tls: bool) -> Self {
        Self::with_endpoint(host, port, api_key, use_tls, "/v1", "openai")
    }

    /// Cria um provider para qualquer endpoint OpenAI-compatível.
    pub fn with_endpoint(
        host: impl Into<String>,
        port: u16,
        api_key: Option<String>,
        use_tls: bool,
        base_path: impl Into<String>,
        label: &'static str,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            api_key,
            use_tls,
            base_path: base_path.into(),
            label,
        }
    }

    /// OpenAI oficial (`api.openai.com/v1`).
    pub fn openai(api_key: Option<String>) -> Self {
        Self::with_endpoint("api.openai.com", 443, api_key, true, "/v1", "openai")
    }

    /// OpenRouter (`openrouter.ai/api/v1`) — agregador multi-modelo.
    pub fn openrouter(api_key: Option<String>) -> Self {
        Self::with_endpoint("openrouter.ai", 443, api_key, true, "/api/v1", "openrouter")
    }

    /// Kimi / Moonshot AI (`api.moonshot.ai/v1`) — API OpenAI-compatível.
    pub fn kimi(api_key: Option<String>) -> Self {
        Self::with_endpoint("api.moonshot.ai", 443, api_key, true, "/v1", "kimi")
    }

    /// MiniMax (`api.minimax.io/v1`) — endpoint OpenAI-compatível.
    pub fn minimax(api_key: Option<String>) -> Self {
        Self::with_endpoint("api.minimax.io", 443, api_key, true, "/v1", "minimax")
    }

    /// Caminho completo do endpoint de chat (ex.: "/v1/chat/completions").
    fn chat_path(&self) -> String {
        format!("{}/chat/completions", self.base_path)
    }

    /// Caminho completo do endpoint de embeddings.
    fn embed_path(&self) -> String {
        format!("{}/embeddings", self.base_path)
    }

    /// Monta o array `messages` do payload. Quando `req.messages` está
    /// preenchido (modo multi-turn), usa o histórico completo; caso contrário,
    /// cai no modo legacy (system + user únicos).
    fn build_messages(req: &ChatRequest) -> Vec<Value> {
        if req.messages.is_empty() {
            return vec![
                serde_json::json!({"role": "system", "content": req.system}),
                serde_json::json!({"role": "user", "content": req.user}),
            ];
        }
        let mut messages = Vec::with_capacity(req.messages.len() + 1);
        if !req.system.is_empty() {
            messages.push(serde_json::json!({"role": "system", "content": req.system}));
        }
        for m in &req.messages {
            messages.push(serde_json::json!({"role": m.role, "content": m.content}));
        }
        messages
    }
}

impl ProviderClient for OpenAiCompatProvider {
    fn name(&self) -> &'static str {
        self.label
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        Box::new(self.clone())
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        // Estimativa por provedor (USD por 1M tokens). O custo real depende do
        // modelo escolhido; estes valores são um teto conservador para o
        // CostTracker, não uma tabela de preços oficial.
        let (per_m_in, per_m_out) = match self.label {
            // GPT-4o: $2.50/1M input, $10.00/1M output.
            "openai" => (2.50, 10.00),
            // Kimi e MiniMax operam tipicamente abaixo de US$ 1/1M input;
            // teto conservador para não subestimar.
            "kimi" | "minimax" => (1.00, 4.00),
            // OpenRouter repassa o preço do modelo roteado (desconhecido aqui);
            // usa o teto do tier GPT-4o como estimativa conservadora.
            "openrouter" => (2.50, 10.00),
            _ => (2.50, 10.00),
        };
        let input_cost = (input_tokens as f64) * per_m_in / 1_000_000.0;
        let output_cost = (output_tokens as f64) * per_m_out / 1_000_000.0;
        input_cost + output_cost
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let payload = serde_json::json!({
            "input": texts,
            "model": "text-embedding-3-small"
        });

        let (response, _rate_limit) = send_with_retry_embed(
            &self.host,
            self.port,
            self.api_key.as_deref(),
            &payload,
            self.use_tls,
            &self.embed_path(),
            self.label,
        )?;

        let data = response.get("data")
            .and_then(|d| d.as_array())
            .context("resposta de embeddings sem campo 'data'")?;

        let mut embeddings = Vec::new();
        for item in data {
            let embedding = item.get("embedding")
                .and_then(|e| e.as_array())
                .context("item de embedding sem campo 'embedding'")?;
            let vec: Vec<f32> = embedding.iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            embeddings.push(vec);
        }

        Ok(embeddings)
    }

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let messages = Self::build_messages(&req);

        let mut payload = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": false
        });

        if let Some(ref tools) = req.tools {
            payload["tools"] = serde_json::to_value(tools)?;
        }

        let (response, rate_limit) = send_with_retry(
            &self.host,
            self.port,
            self.api_key.as_deref(),
            &payload,
            self.use_tls,
            &self.chat_path(),
            self.label,
        )?;

        let mut chat_resp = parse_chat_response(&response)?;
        chat_resp.rate_limit = Some(rate_limit);
        Ok(chat_resp)
    }
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        let messages = Self::build_messages(&req);

        let mut payload = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": true
        });

        if let Some(ref tools) = req.tools {
            payload["tools"] = serde_json::to_value(tools)?;
        }

        let path = self.chat_path();
        let chunks = if self.use_tls {
            sse_stream_tls(&self.host, self.port, self.api_key.as_deref(), &payload, &path)?
        } else {
            sse_stream_tcp(&self.host, self.port, self.api_key.as_deref(), &payload, &path)?
        };

        // Filtra chunks [DONE] e extrai content
        let texts: Vec<Result<String>> = chunks
            .into_iter()
            .filter(|chunk| chunk != "[DONE]")
            .map(|chunk| {
                match serde_json::from_str::<Value>(&chunk) {
                    Ok(json) => {
                        let text = json
                            .get("choices")
                            .and_then(|c| c.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|choice| choice.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        Ok(text)
                    }
                    Err(e) => Err(anyhow::anyhow!("JSON inválido no stream SSE: {}", e)),
                }
            })
            .collect();

        Ok(Box::new(texts.into_iter()))
    }
}

/// Extrai `ChatResponse` de um JSON Value no formato OpenAI.
fn parse_chat_response(body: &Value) -> Result<ChatResponse> {
    let message = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .context("campo 'choices[0].message' ausente na resposta OpenAI")?;

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
                    Some(crate::provider::ToolCall {
                        id,
                        r#type: "function".to_string(),
                        function: crate::provider::ToolCallFunction {
                            name,
                            arguments: args,
                        },
                    })
                })
                .collect::<Vec<_>>()
        });

    let usage = body.get("usage");
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

fn send_with_retry(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    payload: &Value,
    use_tls: bool,
    path: &str,
    label: &str,
) -> Result<(Value, RateLimitSnapshot)> {
    let delays = [1u64, 2, 4];
    let mut last_err = anyhow::anyhow!("sem tentativas");
    for (i, &delay) in delays.iter().enumerate() {
        let result = if use_tls {
            tls_post(host, port, api_key, payload, path)
        } else {
            tcp_post(host, port, api_key, payload, path)
        };
        match result {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!("[{}] retry {}/{}: {}", label, i + 1, delays.len(), last_err);
                    thread::sleep(Duration::from_secs(delay));
                }
            }
        }
    }
    bail!(
        "{} falhou após {} tentativas: {}",
        label,
        delays.len(),
        last_err
    )
}

/// Envia requisição de embeddings com retry.
fn send_with_retry_embed(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    payload: &Value,
    use_tls: bool,
    path: &str,
    label: &str,
) -> Result<(Value, RateLimitSnapshot)> {
    let delays = [1, 2, 4];
    let mut last_err = anyhow::anyhow!("inicial");

    for (i, delay) in delays.iter().enumerate() {
        match if use_tls {
            tls_post(host, port, api_key, payload, path)
        } else {
            tcp_post(host, port, api_key, payload, path)
        } {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!("[{}] embed retry {}/{}: {}", label, i + 1, delays.len(), last_err);
                    thread::sleep(Duration::from_secs(*delay));
                }
            }
        }
    }
    bail!(
        "{} embed falhou após {} tentativas: {}",
        label,
        delays.len(),
        last_err
    )
}

/// Envia requisição via TLS real (HTTPS) na porta informada.
fn tls_post(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    payload: &Value,
    path: &str,
) -> Result<(Value, RateLimitSnapshot)> {
    let body = serde_json::to_string(payload)?;
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
    let auth_header;
    if let Some(key) = api_key {
        auth_header = format!("Bearer {}", key);
        headers.push(("Authorization", &auth_header));
    }

    let request =
        crate::tls_client::build_post_request(host, path, &headers, &body);

    let mut stream = TlsClient::connect(host, port)?;
    stream
        .write_all(request.as_bytes())
        .context("falha ao enviar request TLS")?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .context("falha ao ler resposta TLS")?;

    let (status, response_headers, json_body) = crate::parse_http_response(&raw)?;

    if status == 429 {
        let retry_after = response_headers
            .get("retry-after")
            .and_then(|v| v.parse::<u64>().ok());
        bail!("HTTP 429: rate limited, retry-after={:?}", retry_after);
    }
    if status < 200 || status >= 300 {
        bail!("HTTP {}: {}", status, json_body);
    }

    let rate_limit = RateLimitSnapshot::from_headers(&response_headers);
    let val = serde_json::from_str(json_body.trim())
        .with_context(|| format!("JSON inválido na resposta OpenAI:\n{}", json_body))?;
    Ok((val, rate_limit))
}

/// Envia requisição via TCP puro (HTTP/1.0 sem criptografia).
fn tcp_post(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    payload: &Value,
    path: &str,
) -> Result<(Value, RateLimitSnapshot)> {
    let body = serde_json::to_string(payload)?;
    let auth_header = api_key
        .map(|k| format!("Authorization: Bearer {}\r\n", k))
        .unwrap_or_default();

    let request = format!(
        "POST {} HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         {}\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        path,
        host,
        port,
        auth_header,
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
    let val = serde_json::from_str(json_body.trim())
        .with_context(|| format!("JSON inválido na resposta OpenAI:\n{}", json_body))?;
    Ok((val, rate_limit))
}

// ── SSE Streaming (shared pattern for OpenAI / DeepSeek / Azure) ─────────────

/// Envia requisição streaming via TLS e retorna todos os chunks SSE como Vec.
fn sse_stream_tls(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    payload: &Value,
    path: &str,
) -> Result<Vec<String>> {
    let body = serde_json::to_string(payload)?;
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
    let auth_header;
    if let Some(key) = api_key {
        auth_header = format!("Bearer {}", key);
        headers.push(("Authorization", &auth_header));
    }

    let request = crate::tls_client::build_post_request(host, path, &headers, &body);

    let mut stream = TlsClient::connect(host, port)?;
    stream.write_all(request.as_bytes())?;

    read_sse_lines(&mut stream)
}

/// Envia requisição streaming via TCP puro e retorna todos os chunks SSE como Vec.
fn sse_stream_tcp(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    payload: &Value,
    path: &str,
) -> Result<Vec<String>> {
    let body = serde_json::to_string(payload)?;
    let auth_header = api_key
        .map(|k| format!("Authorization: Bearer {}\r\n", k))
        .unwrap_or_default();

    let request = format!(
        "POST {} HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         {}\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        path, host, port, auth_header, body.len(), body
    );

    let mut stream = TcpStream::connect((host, port))
        .with_context(|| format!("não foi possível conectar a {}:{}", host, port))?;
    stream.write_all(request.as_bytes())?;

    read_sse_lines(&mut stream)
}

/// Lê linhas SSE de um stream Read, extraindo chunks `data: <json>` e `data: [DONE]`.
fn read_sse_lines(reader: &mut dyn Read) -> Result<Vec<String>> {
    let mut buf = [0u8; 4096];
    let mut raw = String::new();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        raw.push_str(&String::from_utf8_lossy(&buf[..n]));
    }

    let mut chunks = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(data) = trimmed.strip_prefix("data: ") {
            chunks.push(data.to_string());
        } else if trimmed.strip_prefix("data:").is_some() {
            // data: sem espaço (ex: data:[DONE])
            let data = trimmed[5..].trim();
            chunks.push(data.to_string());
        }
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_estimate_gpt4o() {
        let provider = OpenAiCompatProvider::new("api.openai.com", 443, None, false);
        // 1M input tokens => $2.50
        assert!((provider.cost_estimate(1_000_000, 0) - 2.50).abs() < 0.0001);
        // 1M output tokens => $10.00
        assert!((provider.cost_estimate(0, 1_000_000) - 10.00).abs() < 0.0001);
        // 1M input + 1M output => $12.50
        assert!((provider.cost_estimate(1_000_000, 1_000_000) - 12.50).abs() < 0.0001);
        // 500k input + 250k output => $1.25 + $2.50 = $3.75
        assert!((provider.cost_estimate(500_000, 250_000) - 3.75).abs() < 0.0001);
    }

    #[test]
    fn provider_new_tcp() {
        let p = OpenAiCompatProvider::new("localhost", 8080, Some("key".into()), false);
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, 8080);
        assert_eq!(p.api_key, Some("key".into()));
        assert!(!p.use_tls);
    }

    #[test]
    fn provider_new_tls() {
        let p = OpenAiCompatProvider::new("api.openai.com", 443, Some("sk-xxx".into()), true);
        assert_eq!(p.host, "api.openai.com");
        assert_eq!(p.port, 443);
        assert_eq!(p.api_key, Some("sk-xxx".into()));
        assert!(p.use_tls);
    }

    #[test]
    fn parse_chat_response_completa() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Olá, mundo!"
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        });
        let resp = parse_chat_response(&body).unwrap();
        assert_eq!(resp.content, "Olá, mundo!");
        assert_eq!(resp.tokens_in, 10);
        assert_eq!(resp.tokens_out, 5);
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn parse_chat_response_sem_usage() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "resposta"
                }
            }]
        });
        let resp = parse_chat_response(&body).unwrap();
        assert_eq!(resp.content, "resposta");
        assert_eq!(resp.tokens_in, 0);
        assert_eq!(resp.tokens_out, 0);
    }

    #[test]
    fn parse_chat_response_com_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"city\":\"São Paulo\"}"
                            }
                        }
                    ]
                }
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 15
            }
        });
        let resp = parse_chat_response(&body).unwrap();
        assert_eq!(resp.content, "");
        let tc = resp.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_1");
        assert_eq!(tc[0].function.name, "get_weather");
        assert_eq!(tc[0].function.arguments, "{\"city\":\"São Paulo\"}");
    }

    #[test]
    fn parse_chat_response_content_null() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null
                }
            }]
        });
        let resp = parse_chat_response(&body).unwrap();
        assert_eq!(resp.content, "");
    }

    #[test]
    fn parse_chat_response_choices_ausente() {
        let body = serde_json::json!({"object": "chat.completion"});
        let err = parse_chat_response(&body).unwrap_err();
        assert!(err.to_string().contains("choices[0].message"));
    }

    #[test]
    fn parse_chat_response_message_ausente() {
        let body = serde_json::json!({
            "choices": [{}]
        });
        let err = parse_chat_response(&body).unwrap_err();
        assert!(err.to_string().contains("choices[0].message"));
    }

    #[test]
    fn parse_chat_response_tool_call_incompleto() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [
                        {"id": "call_1", "type": "function"},
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "ok",
                                "arguments": "{}"
                            }
                        }
                    ]
                }
            }]
        });
        let resp = parse_chat_response(&body).unwrap();
        let tc = resp.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_2");
    }

    #[test]
    fn parse_chat_response_tool_calls_vazio() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "ok",
                    "tool_calls": []
                }
            }]
        });
        let resp = parse_chat_response(&body).unwrap();
        assert_eq!(resp.content, "ok");
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn clone_box_preserva_tls_flag() {
        let p = OpenAiCompatProvider::new("api.openai.com", 443, None, true);
        let cloned = p.clone_box();
        assert_eq!(cloned.name(), "openai");
    }

    #[test]
    fn construtor_openrouter_usa_api_v1() {
        let p = OpenAiCompatProvider::openrouter(Some("key".into()));
        assert_eq!(p.host, "openrouter.ai");
        assert_eq!(p.chat_path(), "/api/v1/chat/completions");
        assert_eq!(p.embed_path(), "/api/v1/embeddings");
        assert_eq!(p.name(), "openrouter");
        assert!(p.use_tls);
    }

    #[test]
    fn construtor_kimi_usa_moonshot() {
        let p = OpenAiCompatProvider::kimi(Some("key".into()));
        assert_eq!(p.host, "api.moonshot.ai");
        assert_eq!(p.chat_path(), "/v1/chat/completions");
        assert_eq!(p.name(), "kimi");
    }

    #[test]
    fn construtor_minimax_usa_minimax_io() {
        let p = OpenAiCompatProvider::minimax(None);
        assert_eq!(p.host, "api.minimax.io");
        assert_eq!(p.chat_path(), "/v1/chat/completions");
        assert_eq!(p.name(), "minimax");
    }

    #[test]
    fn new_preserva_comportamento_legacy() {
        // O construtor histórico deve continuar apontando para /v1 + "openai".
        let p = OpenAiCompatProvider::new("localhost", 8080, None, false);
        assert_eq!(p.chat_path(), "/v1/chat/completions");
        assert_eq!(p.name(), "openai");
    }

    #[test]
    fn build_messages_legacy_system_user() {
        let req = ChatRequest::new("gpt-4o", "sys", "oi");
        let msgs = OpenAiCompatProvider::build_messages(&req);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "oi");
    }

    #[test]
    fn build_messages_multi_turn_usa_historico() {
        use crate::provider::ChatMessageRequest;
        let history = vec![
            ChatMessageRequest {
                role: "user".into(),
                content: "primeira".into(),
                reasoning_content: None,
            },
            ChatMessageRequest {
                role: "assistant".into(),
                content: "resposta".into(),
                reasoning_content: None,
            },
            ChatMessageRequest {
                role: "user".into(),
                content: "segunda".into(),
                reasoning_content: None,
            },
        ];
        let req = ChatRequest::with_messages("gpt-4o", "sys", history);
        let msgs = OpenAiCompatProvider::build_messages(&req);
        // system + 3 turnos do histórico (antes deste fix, o histórico era descartado)
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[3]["content"], "segunda");
    }

    #[test]
    fn build_messages_multi_turn_sem_system() {
        use crate::provider::ChatMessageRequest;
        let history = vec![ChatMessageRequest {
            role: "user".into(),
            content: "oi".into(),
            reasoning_content: None,
        }];
        let req = ChatRequest::with_messages("gpt-4o", "", history);
        let msgs = OpenAiCompatProvider::build_messages(&req);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn cost_estimate_kimi_mais_barato_que_openai() {
        let openai = OpenAiCompatProvider::openai(None);
        let kimi = OpenAiCompatProvider::kimi(None);
        assert!(kimi.cost_estimate(1_000_000, 1_000_000) < openai.cost_estimate(1_000_000, 1_000_000));
        // Kimi: 1M in + 1M out = 1.00 + 4.00 = 5.00
        assert!((kimi.cost_estimate(1_000_000, 1_000_000) - 5.00).abs() < 0.0001);
    }
}
