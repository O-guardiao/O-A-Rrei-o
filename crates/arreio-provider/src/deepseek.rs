//! Provedor DeepSeek (V4-Pro, V4-Flash, R1/V3 legacy).
//!
//! A API DeepSeek é compatível com OpenAI (`/v1/chat/completions`) via HTTPS.
//! A principal diferença é o tratamento de reasoning/thinking:
//!
//! - **V4-Pro/V4-Flash**: retorna thinking como items dentro do array `content[]`
//!   com `type: "thinking"` e `type: "output"`. O `reasoning_content` DEVE ser
//!   ecoado de volta em turnos subsequentes após tool calls.
//! - **Legacy (R1/V3)**: retorna `reasoning_content` como campo top-level.
//!   NÃO deve ser ecoado de volta (causa HTTP 400 se presente).
//!
//! Este provider normaliza ambos os formatos para o campo `reasoning_content`
//! do `ChatResponse`, e gerencia o eco correto em multi-turn.

use crate::provider::{ChatRequest, ChatResponse, ToolCall, ToolCallFunction};
use crate::rate_guard::RateLimitSnapshot;
use crate::tls_client::TlsClient;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

/// Host padrão da API DeepSeek.
const DEFAULT_HOST: &str = "api.deepseek.com";
const DEFAULT_PORT: u16 = 443;

/// Modelos V4 que exigem eco de reasoning_content em multi-turn.
const V4_REASONING_MODELS: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];

/// Modelos legacy que NÃO aceitam reasoning_content na request.
const LEGACY_NO_REASONING_MODELS: &[&str] = &["deepseek-reasoner", "deepseek-r1"];

/// Provedor DeepSeek (V4-Pro, V4-Flash, R1/V3) via HTTPS.
#[derive(Clone)]
pub struct DeepseekProvider {
    host: String,
    port: u16,
    api_key: Option<String>,
    /// Prefixo opcional de base path (ex: "/betteraideepseek" para proxies).
    base_path: String,
    /// Se true, sempre injeta reasoning_content nas mensagens assistant
    /// (necessário para V4 multi-turn com tool calls).
    force_reasoning_echo: bool,
}

impl DeepseekProvider {
    /// Cria um novo provider DeepSeek com host e API key padrão.
    ///
    /// Usa `api.deepseek.com:443` via TLS.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            api_key: Some(api_key.into()),
            base_path: String::new(),
            force_reasoning_echo: false,
        }
    }

    /// Cria provider com URL base customizada (ex: proxies, OpenRouter).
    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Define uma porta customizada.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Define prefixo de path (ex: "/betteraideepseek").
    pub fn with_base_path(mut self, path: impl Into<String>) -> Self {
        self.base_path = path.into();
        self
    }

    /// Força injeção de reasoning_content nas mensagens assistant durante multi-turn.
    /// Necessário para V4-Pro/V4-Flash após tool calls.
    pub fn with_force_reasoning_echo(mut self, force: bool) -> Self {
        self.force_reasoning_echo = force;
        self
    }

    /// Detecta se o modelo é V4 (precisa de reasoning echo).
    fn is_v4_model(model: &str) -> bool {
        let lower = model.to_lowercase();
        V4_REASONING_MODELS
            .iter()
            .any(|m| lower.contains(m))
            || (lower.contains("deepseek") && lower.contains("v4"))
    }

    /// Detecta se o modelo é legacy (NÃO aceita reasoning_content).
    fn is_legacy_no_reasoning(model: &str) -> bool {
        let lower = model.to_lowercase();
        LEGACY_NO_REASONING_MODELS.iter().any(|m| lower.contains(m))
    }

    /// Monta o path completo da API.
    fn api_path(&self) -> String {
        if self.base_path.is_empty() {
            "/v1/chat/completions".to_string()
        } else {
            format!("{}/v1/chat/completions", self.base_path.trim_end_matches('/'))
        }
    }

    /// Constrói o array de mensagens JSON incluindo reasoning_content quando necessário.
    fn build_messages(req: &ChatRequest) -> Vec<Value> {
        if req.messages.is_empty() {
            // Modo legacy: system + user únicos
            let msgs = vec![
                serde_json::json!({"role": "system", "content": req.system}),
                serde_json::json!({"role": "user", "content": req.user}),
            ];
            msgs
        } else {
            // Modo multi-turn
            let mut msgs: Vec<Value> = Vec::new();
            // System message primeiro
            if !req.system.is_empty() {
                msgs.push(serde_json::json!({"role": "system", "content": req.system}));
            }
            for m in &req.messages {
                let mut msg = serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                });
                // Injeta reasoning_content se presente e modelo NÃO é legacy
                if let Some(ref rc) = m.reasoning_content {
                    if !rc.is_empty() && !Self::is_legacy_no_reasoning(&req.model) {
                        msg["reasoning_content"] = serde_json::json!(rc);
                    }
                }
                msgs.push(msg);
            }
            msgs
        }
    }
}

impl crate::provider::ProviderClient for DeepseekProvider {
    fn name(&self) -> &'static str {
        "deepseek"
    }

    fn clone_box(&self) -> Box<dyn crate::provider::ProviderClient> {
        Box::new(self.clone())
    }

    fn cost_estimate(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        // DeepSeek V4-Pro: $0.28/1M input, $1.10/1M output (preços competitivos).
        // Cache hit no input: $0.07/1M.
        let input_cost = (input_tokens as f64) * 0.28 / 1_000_000.0;
        let output_cost = (output_tokens as f64) * 1.10 / 1_000_000.0;
        input_cost + output_cost
    }

    fn embed(&self, _texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        Err(anyhow::anyhow!(
            "embed não implementado para DeepSeek"
        ))
    }

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let model = req.model.clone();
        let messages = Self::build_messages(&req);

        let mut payload = serde_json::json!({
            "model": &model,
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
            &self.api_path(),
            &payload,
        )?;

        let mut chat_resp = parse_deepseek_response(&response, &model)?;
        chat_resp.rate_limit = Some(rate_limit);
        Ok(chat_resp)
    }

    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Iterator<Item = Result<String>> + Send>> {
        let model = req.model.clone();
        let messages = Self::build_messages(&req);

        let mut payload = serde_json::json!({
            "model": &model,
            "messages": messages,
            "stream": true
        });

        if let Some(ref tools) = req.tools {
            payload["tools"] = serde_json::to_value(tools)?;
        }

        let body = serde_json::to_string(&payload)?;
        let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
        let auth_header;
        if let Some(ref key) = self.api_key {
            auth_header = format!("Bearer {}", key);
            headers.push(("Authorization", &auth_header));
        }

        let path = self.api_path();
        let request = crate::tls_client::build_post_request(
            &self.host,
            &path,
            &headers,
            &body,
        );

        let mut stream = TlsClient::connect(&self.host, self.port)?;
        stream.write_all(request.as_bytes())?;

        // DeepSeek usa formato SSE OpenAI-compatível
        let raw_chunks = read_sse_lines_deepseek(&mut stream)?;
        let texts: Vec<Result<String>> = raw_chunks
            .into_iter()
            .filter(|chunk| chunk != "[DONE]")
            .map(|chunk: String| {
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
                    Err(e) => Err(anyhow::anyhow!("JSON inválido no stream DeepSeek: {}", e)),
                }
            })
            .collect();

        Ok(Box::new(texts.into_iter()))
    }
}

// ── SSE Streaming helpers ────────────────────────────────────────────────────

/// Lê chunks SSE do DeepSeek (formato OpenAI-compatible: data: JSON)
fn read_sse_lines_deepseek(reader: &mut dyn std::io::Read) -> Result<Vec<String>> {
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
            texts.push(data.to_string());
        } else if let Some(data) = trimmed.strip_prefix("data:") {
            texts.push(data.trim().to_string());
        }
    }
    Ok(texts)
}

/// Faz parsing da resposta DeepSeek, normalizando reasoning_content.
///
/// Trata dois formatos:
/// 1. **V4 format**: `content[]` array com `type: "thinking"` e `type: "output"`
/// 2. **Legacy format**: `content` string + `reasoning_content` campo top-level
fn parse_deepseek_response(body: &Value, model: &str) -> Result<ChatResponse> {
    let choice = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .context("campo 'choices[0]' ausente na resposta DeepSeek")?;

    let message = choice
        .get("message")
        .context("campo 'message' ausente na resposta DeepSeek")?;

    // ── Extrai content e reasoning ──────────────────────────────────────
    let (content, reasoning) = if DeepseekProvider::is_v4_model(model) {
        parse_v4_content(message)
    } else {
        parse_legacy_content(message)
    };

    // ── Extrai tool calls ───────────────────────────────────────────────
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
                    let args = func
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}")
                        .to_string();
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

    // ── Extrai usage ────────────────────────────────────────────────────
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
        tool_calls: if tool_calls
            .as_ref()
            .map(|v| v.is_empty())
            .unwrap_or(true)
        {
            None
        } else {
            tool_calls
        },
        tokens_in,
        tokens_out,
        rate_limit: None,
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
    })
}

/// Extrai content e thinking do formato V4 (`content[]` array).
///
/// Exemplo de response V4:
/// ```json
/// "content": [
///   {"type": "thinking", "thinking": "Raciocínio interno..."},
///   {"type": "output", "output": "Resposta final"}
/// ]
/// ```
fn parse_v4_content(message: &Value) -> (String, String) {
    let content_field = message.get("content");

    // Se content é array, percorre items por type
    if let Some(arr) = content_field.and_then(|c| c.as_array()) {
        let mut thinking = String::new();
        let mut output = String::new();

        for item in arr {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("thinking") => {
                    if let Some(t) = item.get("thinking").and_then(|v| v.as_str()) {
                        thinking.push_str(t);
                    }
                }
                Some("output") => {
                    if let Some(o) = item.get("output").and_then(|v| v.as_str()) {
                        output.push_str(o);
                    }
                }
                _ => {}
            }
        }

        (output, thinking)
    } else {
        // Fallback: content é string (formato compatível)
        let content = content_field
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let reasoning = message
            .get("reasoning_content")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        (content, reasoning)
    }
}

/// Extrai content e reasoning do formato legacy (R1/V3).
///
/// Formato:
/// ```json
/// "content": "Resposta final",
/// "reasoning_content": "Raciocínio interno..."
/// ```
fn parse_legacy_content(message: &Value) -> (String, String) {
    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let reasoning = message
        .get("reasoning_content")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    (content, reasoning)
}

/// Envia requisição HTTPS para a API DeepSeek com retry.
fn send_with_retry(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    api_path: &str,
    payload: &Value,
) -> Result<(Value, RateLimitSnapshot)> {
    let delays = [1u64, 2, 4];
    let mut last_err = anyhow::anyhow!("sem tentativas");

    for (i, &delay) in delays.iter().enumerate() {
        match tls_post(host, port, api_key, api_path, payload) {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!(
                        "[deepseek] retry {}/{}: {}",
                        i + 1,
                        delays.len(),
                        last_err
                    );
                    thread::sleep(Duration::from_secs(delay));
                }
            }
        }
    }

    bail!(
        "DeepSeek falhou após {} tentativas: {}",
        delays.len(),
        last_err
    )
}

/// Envia requisição HTTPS para a API DeepSeek.
fn tls_post(
    host: &str,
    port: u16,
    api_key: Option<&str>,
    api_path: &str,
    payload: &Value,
) -> Result<(Value, RateLimitSnapshot)> {
    let body = serde_json::to_string(payload)?;
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json")];
    let auth_header;
    if let Some(key) = api_key {
        auth_header = format!("Bearer {}", key);
        headers.push(("Authorization", &auth_header));
    }

    let request = crate::tls_client::build_post_request(host, api_path, &headers, &body);

    let mut stream = TlsClient::connect(host, port)
        .with_context(|| format!("falha de conexão TLS para {}", host))?;

    stream
        .write_all(request.as_bytes())
        .context("falha ao enviar request TLS DeepSeek")?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .context("falha ao ler resposta TLS DeepSeek")?;

    let (status, response_headers, json_body) = crate::parse_http_response(&raw)?;

    if status == 429 {
        let retry_after = response_headers
            .get("retry-after")
            .and_then(|v| v.parse::<u64>().ok());
        bail!("HTTP 429: rate limited, retry-after={:?}", retry_after);
    }
    // DeepSeek pode retornar 400 quando reasoning_content viola regras de eco.
    if status == 400 {
        if json_body.contains("reasoning_content") {
            bail!(
                "HTTP 400 reasoning_content: verifique eco do campo entre turnos. \
                 V4 exige reasoning_content após tool calls; R1 proíbe. \
                 Body: {}",
                json_body
            );
        }
        bail!("HTTP 400: {}", json_body);
    }
    if status < 200 || status >= 300 {
        bail!("HTTP {}: {}", status, json_body);
    }

    let rate_limit = RateLimitSnapshot::from_headers(&response_headers);
    let val = serde_json::from_str(json_body.trim())
        .with_context(|| format!("JSON inválido na resposta DeepSeek:\n{}", json_body))?;
    Ok((val, rate_limit))
}

// ── Roteador de Modelos DeepSeek ──────────────────────────────────────────────
//
// Seleciona automaticamente o modelo DeepSeek ideal baseado no tipo de tarefa,
// complexidade e orçamento. Integra-se ao OODA-C: fast-path usa Flash, deep
// deliberation usa Pro.

/// Estratégia de seleção de modelo DeepSeek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepSeekModelStrategy {
    /// Usa V4-Flash: mais barato e rápido, adequado para tarefas simples.
    Flash,
    /// Usa V4-Pro: raciocínio profundo, adequado para tarefas complexas.
    Pro,
    /// Usa V3 legacy: compatibilidade com sistemas legados.
    Legacy,
}

/// Resultado da seleção de modelo.
#[derive(Debug, Clone)]
pub struct ModelSelection {
    /// Nome do modelo a ser usado na API.
    pub model_id: String,
    /// Estratégia de seleção aplicada.
    pub strategy: DeepSeekModelStrategy,
    /// Custo estimado por 1M tokens de input.
    pub cost_per_m_input: f64,
    /// Custo estimado por 1M tokens de output.
    pub cost_per_m_output: f64,
}

/// Roteador de modelos DeepSeek.
///
/// Centraliza a lógica de seleção Pro vs Flash vs Legacy baseada
/// nas características da tarefa.
pub struct DeepSeekModelRouter;

impl DeepSeekModelRouter {
    /// Seleciona o modelo ideal para o tipo de tarefa.
    ///
    /// # Regras:
    /// - `quick_query`, `conversation`, `translation` → Flash (rápido, barato)
    /// - `code_generation`, `refactoring`, `architecture` → Pro (raciocínio profundo)
    /// - `batch_processing`, `summarization` → Flash para bulk econômico
    /// - `analysis`, `debugging`, `planning` → Pro (precisão)
    /// - Tasks com `confidence >= 0.85` → Flash (fast-path OODA-C)
    /// - Tasks com `emergency == true` → Pro (deep deliberation)
    pub fn select(
        task_type: &str,
        confidence: Option<f64>,
        emergency: bool,
    ) -> ModelSelection {
        // Emergência sempre força Pro (deep deliberation)
        if emergency {
            return ModelSelection {
                model_id: "deepseek-v4-pro".to_string(),
                strategy: DeepSeekModelStrategy::Pro,
                cost_per_m_input: 0.28,
                cost_per_m_output: 1.10,
            };
        }

        // Alta confiança no fast-path → Flash
        if let Some(conf) = confidence {
            if conf >= 0.85 {
                return ModelSelection {
                    model_id: "deepseek-v4-flash".to_string(),
                    strategy: DeepSeekModelStrategy::Flash,
                    cost_per_m_input: 0.14,
                    cost_per_m_output: 0.55,
                };
            }
        }

        // Roteamento por tipo de tarefa
        match task_type {
            "quick_query" | "conversation" | "translation" | "simple_chat" => {
                ModelSelection {
                    model_id: "deepseek-v4-flash".to_string(),
                    strategy: DeepSeekModelStrategy::Flash,
                    cost_per_m_input: 0.14,
                    cost_per_m_output: 0.55,
                }
            }
            "batch_processing" | "summarization" | "extraction" => {
                ModelSelection {
                    model_id: "deepseek-v4-flash".to_string(),
                    strategy: DeepSeekModelStrategy::Flash,
                    cost_per_m_input: 0.14,
                    cost_per_m_output: 0.55,
                }
            }
            "legacy_compat" | "v3_compat" => ModelSelection {
                model_id: "deepseek-chat".to_string(),
                strategy: DeepSeekModelStrategy::Legacy,
                cost_per_m_input: 0.14,
                cost_per_m_output: 0.28,
            },
            // Default: Pro para tarefas que exigem raciocínio
            _ => ModelSelection {
                model_id: "deepseek-v4-pro".to_string(),
                strategy: DeepSeekModelStrategy::Pro,
                cost_per_m_input: 0.28,
                cost_per_m_output: 1.10,
            },
        }
    }

    /// Resolve um nome de modelo curto ou alias para o ID canônico.
    pub fn resolve_alias(alias: &str) -> String {
        match alias.to_lowercase().as_str() {
            "pro" | "v4-pro" | "v4pro" | "deepseek-pro" => "deepseek-v4-pro".to_string(),
            "flash" | "v4-flash" | "v4flash" | "deepseek-flash" => "deepseek-v4-flash".to_string(),
            "r1" | "reasoner" | "legacy" => "deepseek-reasoner".to_string(),
            "v3" | "chat" => "deepseek-chat".to_string(),
            other => other.to_string(),
        }
    }
}

// ── Testes ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessageRequest, ChatRequest, ProviderClient};

    // ── Fixtures ────────────────────────────────────────────────────────

    /// Fixture: resposta V4-Pro completa com content[] array (thinking + output).
    fn fixture_v4_pro_with_tools() -> Value {
        serde_json::json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion",
            "created": 1720000000,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "O usuário quer criar um arquivo. Preciso usar a ferramenta write_file. Vou verificar o caminho primeiro."},
                        {"type": "output", "output": "Vou criar o arquivo para você."}
                    ],
                    "tool_calls": [
                        {
                            "id": "call_abc123",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"path\":\"/tmp/test.txt\",\"content\":\"Hello World\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 150,
                "completion_tokens": 80,
                "total_tokens": 230
            }
        })
    }

    /// Fixture: resposta V4-Pro simples sem tool calls.
    fn fixture_v4_pro_simple() -> Value {
        serde_json::json!({
            "id": "chatcmpl-def456",
            "object": "chat.completion",
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "Pergunta simples, resposta direta."},
                        {"type": "output", "output": "Olá! Como posso ajudar?"}
                    ]
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 25,
                "total_tokens": 35
            }
        })
    }

    /// Fixture: resposta legacy (R1/V3) com reasoning_content top-level.
    fn fixture_legacy_with_reasoning() -> Value {
        serde_json::json!({
            "id": "chatcmpl-ghi789",
            "object": "chat.completion",
            "model": "deepseek-reasoner",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "A resposta é 42.",
                    "reasoning_content": "Vamos analisar... 6 * 7 = 42. Portanto a resposta é 42."
                }
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 30,
                "total_tokens": 50
            }
        })
    }

    /// Fixture: resposta V4-Flash simples.
    fn fixture_v4_flash_simple() -> Value {
        serde_json::json!({
            "id": "chatcmpl-jkl012",
            "object": "chat.completion",
            "model": "deepseek-v4-flash",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Resposta rápida."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 3,
                "total_tokens": 8
            }
        })
    }

    // ── Testes: Provider ────────────────────────────────────────────────

    #[test]
    fn provider_name() {
        let p = DeepseekProvider::new("sk-test");
        assert_eq!(p.name(), "deepseek");
    }

    #[test]
    fn cost_estimate() {
        let p = DeepseekProvider::new("sk-test");
        // 1M input tokens => $0.28
        let cost = p.cost_estimate(1_000_000, 0);
        assert!((cost - 0.28).abs() < 0.001);
        // 1M output tokens => $1.10
        let cost = p.cost_estimate(0, 1_000_000);
        assert!((cost - 1.10).abs() < 0.001);
        // 1M in + 1M out => $1.38
        let cost = p.cost_estimate(1_000_000, 1_000_000);
        assert!((cost - 1.38).abs() < 0.001);
    }

    #[test]
    fn is_v4_model_detection() {
        assert!(DeepseekProvider::is_v4_model("deepseek-v4-pro"));
        assert!(DeepseekProvider::is_v4_model("deepseek-v4-flash"));
        assert!(DeepseekProvider::is_v4_model("deepseek-v4"));
        assert!(!DeepseekProvider::is_v4_model("deepseek-reasoner"));
        assert!(!DeepseekProvider::is_v4_model("deepseek-r1"));
        assert!(!DeepseekProvider::is_v4_model("gpt-4o"));
    }

    #[test]
    fn is_legacy_no_reasoning_detection() {
        assert!(DeepseekProvider::is_legacy_no_reasoning("deepseek-reasoner"));
        assert!(DeepseekProvider::is_legacy_no_reasoning("deepseek-r1"));
        assert!(!DeepseekProvider::is_legacy_no_reasoning("deepseek-v4-pro"));
        assert!(!DeepseekProvider::is_legacy_no_reasoning("deepseek-v4-flash"));
    }

    // ── Testes: parse_deepseek_response ──────────────────────────────────

    #[test]
    fn parse_v4_pro_with_tools() {
        let resp =
            parse_deepseek_response(&fixture_v4_pro_with_tools(), "deepseek-v4-pro").unwrap();

        // Content deve ser o texto de output
        assert_eq!(resp.content, "Vou criar o arquivo para você.");
        assert!(resp.reasoning_content.is_some());
        let reasoning = resp.reasoning_content.unwrap();
        assert!(reasoning.contains("write_file"));
        assert!(reasoning.contains("caminho"));

        // Tool calls
        let tc = resp.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "write_file");

        // Tokens
        assert_eq!(resp.tokens_in, 150);
        assert_eq!(resp.tokens_out, 80);
    }

    #[test]
    fn parse_v4_pro_simple() {
        let resp =
            parse_deepseek_response(&fixture_v4_pro_simple(), "deepseek-v4-pro").unwrap();

        assert_eq!(resp.content, "Olá! Como posso ajudar?");
        assert!(resp.reasoning_content.is_some());
        let reasoning = resp.reasoning_content.unwrap();
        assert!(reasoning.contains("Pergunta simples"));

        assert!(resp.tool_calls.is_none());
        assert_eq!(resp.tokens_in, 10);
        assert_eq!(resp.tokens_out, 25);
    }

    #[test]
    fn parse_legacy_with_reasoning() {
        let resp =
            parse_deepseek_response(&fixture_legacy_with_reasoning(), "deepseek-reasoner").unwrap();

        assert_eq!(resp.content, "A resposta é 42.");
        assert!(resp.reasoning_content.is_some());
        let reasoning = resp.reasoning_content.unwrap();
        assert!(reasoning.contains("6 * 7 = 42"));

        assert!(resp.tool_calls.is_none());
        assert_eq!(resp.tokens_in, 20);
        assert_eq!(resp.tokens_out, 30);
    }

    #[test]
    fn parse_v4_flash_simple_no_reasoning() {
        let resp =
            parse_deepseek_response(&fixture_v4_flash_simple(), "deepseek-v4-flash").unwrap();

        assert_eq!(resp.content, "Resposta rápida.");
        // Flash pode não incluir thinking
        assert!(resp.reasoning_content.is_none());
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn parse_response_choices_ausente() {
        let body = serde_json::json!({"object": "chat.completion"});
        let err = parse_deepseek_response(&body, "deepseek-v4-pro").unwrap_err();
        assert!(err.to_string().contains("choices[0]"));
    }

    #[test]
    fn parse_response_message_ausente() {
        let body = serde_json::json!({
            "choices": [{"index": 0}]
        });
        let err = parse_deepseek_response(&body, "deepseek-v4-pro").unwrap_err();
        assert!(err.to_string().contains("message"));
    }

    // ── Testes: build_messages ──────────────────────────────────────────

    #[test]
    fn build_messages_legacy_mode() {
        let req = ChatRequest::new("deepseek-v4-pro", "Você é um assistente.", "Olá!");
        let msgs = DeepseekProvider::build_messages(&req);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Você é um assistente.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "Olá!");
    }

    #[test]
    fn build_messages_multi_turn_with_reasoning() {
        let messages = vec![
            ChatMessageRequest {
                role: "user".to_string(),
                content: "Crie um arquivo".to_string(),
                reasoning_content: None,
            },
            ChatMessageRequest {
                role: "assistant".to_string(),
                content: "Vou criar.".to_string(),
                reasoning_content: Some("Preciso usar write_file...".to_string()),
            },
            ChatMessageRequest {
                role: "user".to_string(),
                content: "Obrigado!".to_string(),
                reasoning_content: None,
            },
        ];
        let req = ChatRequest::with_messages("deepseek-v4-pro", "Sistema", messages);
        let msgs = DeepseekProvider::build_messages(&req);

        assert_eq!(msgs.len(), 4); // system + 3 messages
        // Assistant message deve ter reasoning_content injetado
        let assistant = &msgs[2];
        assert_eq!(assistant["role"], "assistant");
        assert!(assistant.get("reasoning_content").is_some());
        assert_eq!(
            assistant["reasoning_content"],
            "Preciso usar write_file..."
        );
    }

    #[test]
    fn build_messages_legacy_no_reasoning_injection() {
        // R1 não deve receber reasoning_content na request
        let messages = vec![ChatMessageRequest {
            role: "assistant".to_string(),
            content: "Resposta".to_string(),
            reasoning_content: Some("Raciocínio...".to_string()),
        }];
        let req = ChatRequest::with_messages("deepseek-reasoner", "Sistema", messages);
        let msgs = DeepseekProvider::build_messages(&req);

        // Deve ter system + 1 assistant, sem reasoning_content
        assert_eq!(msgs.len(), 2);
        let assistant = &msgs[1];
        assert_eq!(assistant["role"], "assistant");
        // reasoning_content NÃO deve estar presente para R1
        assert!(assistant.get("reasoning_content").is_none());
    }

    // ── Testes: parse_v4_content ────────────────────────────────────────

    #[test]
    fn parse_v4_content_with_thinking_and_output() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "Raciocínio aqui."},
                {"type": "output", "output": "Resposta aqui."}
            ]
        });
        let (content, reasoning) = parse_v4_content(&msg);
        assert_eq!(content, "Resposta aqui.");
        assert_eq!(reasoning, "Raciocínio aqui.");
    }

    #[test]
    fn parse_v4_content_string_fallback() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": "Resposta string."
        });
        let (content, reasoning) = parse_v4_content(&msg);
        assert_eq!(content, "Resposta string.");
        assert_eq!(reasoning, "");
    }

    #[test]
    fn parse_v4_content_null_content() {
        let msg = serde_json::json!({
            "role": "assistant",
            "content": null
        });
        let (content, reasoning) = parse_v4_content(&msg);
        assert_eq!(content, "");
        assert_eq!(reasoning, "");
    }

    // ── Testes: ModelRouter ──────────────────────────────────────────────

    #[test]
    fn model_router_quick_query_uses_flash() {
        let sel = DeepSeekModelRouter::select("quick_query", None, false);
        assert_eq!(sel.model_id, "deepseek-v4-flash");
        assert_eq!(sel.strategy, DeepSeekModelStrategy::Flash);
    }

    #[test]
    fn model_router_code_generation_uses_pro() {
        let sel = DeepSeekModelRouter::select("code_generation", None, false);
        assert_eq!(sel.model_id, "deepseek-v4-pro");
        assert_eq!(sel.strategy, DeepSeekModelStrategy::Pro);
    }

    #[test]
    fn model_router_emergency_forces_pro() {
        let sel = DeepSeekModelRouter::select("quick_query", None, true);
        assert_eq!(sel.model_id, "deepseek-v4-pro");
        assert_eq!(sel.strategy, DeepSeekModelStrategy::Pro);
    }

    #[test]
    fn model_router_high_confidence_uses_flash() {
        let sel = DeepSeekModelRouter::select("code_generation", Some(0.90), false);
        assert_eq!(sel.model_id, "deepseek-v4-flash");
        assert_eq!(sel.strategy, DeepSeekModelStrategy::Flash);
    }

    #[test]
    fn model_router_low_confidence_stays_pro() {
        let sel = DeepSeekModelRouter::select("code_generation", Some(0.50), false);
        assert_eq!(sel.model_id, "deepseek-v4-pro");
        assert_eq!(sel.strategy, DeepSeekModelStrategy::Pro);
    }

    #[test]
    fn model_router_unknown_task_defaults_pro() {
        let sel = DeepSeekModelRouter::select("something_else", None, false);
        assert_eq!(sel.model_id, "deepseek-v4-pro");
    }

    #[test]
    fn resolve_alias_pro() {
        assert_eq!(
            DeepSeekModelRouter::resolve_alias("pro"),
            "deepseek-v4-pro"
        );
        assert_eq!(
            DeepSeekModelRouter::resolve_alias("v4pro"),
            "deepseek-v4-pro"
        );
    }

    #[test]
    fn resolve_alias_flash() {
        assert_eq!(
            DeepSeekModelRouter::resolve_alias("flash"),
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn resolve_alias_passthrough() {
        assert_eq!(
            DeepSeekModelRouter::resolve_alias("deepseek-v4-pro"),
            "deepseek-v4-pro"
        );
    }
}
