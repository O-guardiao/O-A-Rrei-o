use crate::provider::{ChatRequest, ChatResponse, ProviderClient};
use crate::rate_guard::RateLimitSnapshot;
use anyhow::{bail, Context, Result};
use arreio_kernel::Blackboard;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OLLAMA_HOST: &str = "127.0.0.1";
const OLLAMA_PORT: u16 = 11434;

#[derive(Clone)]
pub struct OllamaProvider {
    blackboard: Blackboard,
}

impl OllamaProvider {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }
}

impl ProviderClient for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    fn clone_box(&self) -> Box<dyn ProviderClient> {
        Box::new(self.clone())
    }

    fn cost_estimate(&self, _input_tokens: u32, _output_tokens: u32) -> f64 {
        // Ollama roda localmente — sem custo por token.
        0.0
    }

    fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        // Ollama /api/embeddings aceita array de strings no campo `input`.
        let payload = serde_json::json!({
            "model": "nomic-embed-text",
            "input": texts,
        });

        let (response, _rate_limit) = send_with_retry_embed(&payload)?;

        // Ollama retorna um único embedding quando input é uma string;
        // quando input é array, retorna array de embeddings no campo `embeddings`.
        if let Some(embeddings_arr) = response.get("embeddings").and_then(|e| e.as_array()) {
            let mut embeddings = Vec::new();
            for item in embeddings_arr {
                let vec = item.as_array()
                    .context("item de embedding não é array")?
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                embeddings.push(vec);
            }
            Ok(embeddings)
        } else if let Some(embedding) = response.get("embedding").and_then(|e| e.as_array()) {
            // Fallback para resposta de embedding único
            let vec = embedding.iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            Ok(vec![vec])
        } else {
            bail!("resposta de embeddings do Ollama sem campo 'embeddings' ou 'embedding'")
        }
    }

    fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let messages = if req.messages.is_empty() {
            // Modo legacy: system + user únicos
            vec![
                serde_json::json!({"role": "system", "content": req.system}),
                serde_json::json!({"role": "user",   "content": req.user}),
            ]
        } else {
            // Modo multi-turn: monta array de mensagens
            let mut msgs = Vec::new();
            if !req.system.is_empty() {
                msgs.push(serde_json::json!({"role": "system", "content": req.system}));
            }
            for m in &req.messages {
                msgs.push(serde_json::json!({"role": m.role, "content": m.content}));
            }
            msgs
        };

        let mut payload = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": false
        });

        if let Some(ref tools) = req.tools {
            let tools_val = serde_json::to_value(tools)?;
            payload["tools"] = tools_val;
        }

        let (response, rate_limit) = send_with_retry_chat(&payload)?;

        // Métrica de tokens → Blackboard
        let tokens_out = response
            .get("eval_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let tokens_in = response
            .get("prompt_eval_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let _ = self.blackboard.put_tuple(
            "metrics",
            &format!("tokens/{}/{}", self.name(), ts),
            serde_json::json!({"in": tokens_in, "out": tokens_out}),
        );

        let message = response
            .get("message")
            .context("campo 'message' ausente na resposta do Ollama")?;

        let content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        // Parse tool_calls se presentes
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
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| {
                                serde_json::to_string(
                                    func.get("arguments").unwrap_or(&serde_json::json!({})),
                                )
                                .unwrap_or_else(|_| "{}".to_string())
                            });
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

        Ok(ChatResponse {
            content,
            tool_calls: if tool_calls.as_ref().map(|v| v.is_empty()).unwrap_or(true) {
                None
            } else {
                tool_calls
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
        let messages = if req.messages.is_empty() {
            vec![
                serde_json::json!({"role": "system", "content": req.system}),
                serde_json::json!({"role": "user",   "content": req.user}),
            ]
        } else {
            let mut msgs = Vec::new();
            if !req.system.is_empty() {
                msgs.push(serde_json::json!({"role": "system", "content": req.system}));
            }
            for m in &req.messages {
                msgs.push(serde_json::json!({"role": m.role, "content": m.content}));
            }
            msgs
        };

        let mut payload = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": true
        });

        if let Some(ref tools) = req.tools {
            let tools_val = serde_json::to_value(tools)?;
            payload["tools"] = tools_val;
        }

        let body = serde_json::to_string(&payload)?;
        let request = format!(
            "POST /api/chat HTTP/1.0\r\n\
             Host: {}:{}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            OLLAMA_HOST,
            OLLAMA_PORT,
            body.len(),
            body
        );

        let mut stream = TcpStream::connect((OLLAMA_HOST, OLLAMA_PORT))
            .context("Ollama não está rodando em localhost:11434")?;
        stream
            .write_all(request.as_bytes())
            .context("falha ao enviar request HTTP")?;

        // Leitura da resposta HTTP. Para streaming, precisamos ler o corpo
        // linha por linha (NDJSON).
        let mut reader = std::io::BufReader::new(stream);
        let mut headers_done = false;
        let mut header_buf = String::new();

        // Lê headers até linha em branco
        while !headers_done {
            header_buf.clear();
            let n = std::io::BufRead::read_line(&mut reader, &mut header_buf)
                .context("falha ao ler header")?;
            if n == 0 {
                break;
            }
            if header_buf.trim().is_empty() {
                headers_done = true;
            }
        }

        // Cria iterador que lê linhas NDJSON e extrai tokens
        let iter = OllamaStreamIterator { reader };
        Ok(Box::new(iter))
    }
}

fn send_with_retry_chat(payload: &Value) -> Result<(Value, RateLimitSnapshot)> {
    let delays = [1u64, 2, 4];
    let mut last_err = anyhow::anyhow!("sem tentativas");
    for (i, &delay) in delays.iter().enumerate() {
        match tcp_post(payload, "/api/chat") {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!("[ollama] retry {}/{}: {}", i + 1, delays.len(), last_err);
                    thread::sleep(Duration::from_secs(delay));
                }
            }
        }
    }
    bail!(
        "Ollama falhou após {} tentativas: {}",
        delays.len(),
        last_err
    )
}

fn send_with_retry_embed(payload: &Value) -> Result<(Value, RateLimitSnapshot)> {
    let delays = [1u64, 2, 4];
    let mut last_err = anyhow::anyhow!("sem tentativas");
    for (i, &delay) in delays.iter().enumerate() {
        match tcp_post(payload, "/api/embeddings") {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = e;
                if i < delays.len() - 1 {
                    eprintln!("[ollama] embed retry {}/{}: {}", i + 1, delays.len(), last_err);
                    thread::sleep(Duration::from_secs(delay));
                }
            }
        }
    }
    bail!(
        "Ollama embed falhou após {} tentativas: {}",
        delays.len(),
        last_err
    )
}

fn tcp_post(payload: &Value, path: &str) -> Result<(Value, RateLimitSnapshot)> {
    let body = serde_json::to_string(payload)?;
    let request = format!(
        "POST {} HTTP/1.0\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {}",
        path,
        OLLAMA_HOST,
        OLLAMA_PORT,
        body.len(),
        body
    );

    let mut stream = TcpStream::connect((OLLAMA_HOST, OLLAMA_PORT))
        .context("Ollama não está rodando em localhost:11434")?;

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
        .with_context(|| format!("JSON inválido na resposta Ollama:\n{}", json_body))?;
    Ok((val, rate_limit))
}

/// Iterador de streaming para respostas NDJSON do Ollama.
struct OllamaStreamIterator {
    reader: std::io::BufReader<TcpStream>,
}

impl Iterator for OllamaStreamIterator {
    type Item = Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match std::io::BufRead::read_line(&mut self.reader, &mut line) {
            Ok(0) => None, // EOF
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return self.next();
                }
                match serde_json::from_str::<Value>(trimmed) {
                    Ok(json) => {
                        // Verifica se é o chunk final (done=true)
                        if json.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
                            return None;
                        }
                        let content = json
                            .get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(Ok(content))
                    }
                    Err(e) => Some(Err(anyhow::anyhow!("JSON inválido no stream: {}", e))),
                }
            }
            Err(e) => Some(Err(anyhow::anyhow!("falha ao ler stream: {}", e))),
        }
    }
}
