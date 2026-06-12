use anyhow::{bail, Context, Result};
use arreio_kernel::Blackboard;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OLLAMA_HOST: &str = "127.0.0.1";
const OLLAMA_PORT: u16 = 11434;

pub struct OllamaClient {
    blackboard: Blackboard,
}

impl OllamaClient {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Invocação stateless: system_prompt + user_msg → texto gerado.
    /// Registra eval_count no Blackboard (rastreio de tokens por ator).
    pub fn chat(&self, model: &str, actor_name: &str, system: &str, user: &str) -> Result<String> {
        let payload = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user",   "content": user}
            ],
            "stream": false
        });

        let response = self.send_with_retry(&payload)?;

        // Métrica de tokens → Blackboard (categoria "metrics")
        if let Some(count) = response.get("eval_count").and_then(|v| v.as_u64()) {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let _ = self.blackboard.put_tuple(
                "metrics",
                &format!("tokens/{}/{}", actor_name, ts),
                serde_json::json!(count),
            );
        }

        response
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .context("campo 'message.content' ausente na resposta do Ollama")
    }

    // backoff: 1s → 2s → 4s
    fn send_with_retry(&self, payload: &Value) -> Result<Value> {
        let delays = [1u64, 2, 4];
        let mut last_err = anyhow::anyhow!("sem tentativas");
        for (i, &delay) in delays.iter().enumerate() {
            match tcp_post(payload) {
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
}

/// Requisição HTTP/1.0 via TCP puro. Ollama é localhost — não há TLS.
/// Usar stdlib evita dependências pesadas de rede (rustls/ICU).
fn tcp_post(payload: &Value) -> Result<Value> {
    let body = serde_json::to_string(payload)?;
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

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .context("falha ao ler resposta HTTP")?;

    // Extrai corpo JSON após os headers HTTP (\r\n\r\n)
    let body_start = raw
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .context("resposta HTTP inválida: sem separador de headers")?;

    let json_body = &raw[body_start..].trim();
    serde_json::from_str(json_body)
        .with_context(|| format!("JSON inválido na resposta Ollama:\n{}", json_body))
}
