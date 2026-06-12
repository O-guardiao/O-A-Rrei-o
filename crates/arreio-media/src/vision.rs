use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;

/// Capacidade de descrever imagens via modelo vision.
pub trait ImageDescriber {
    fn describe(&self, image_path: &Path, prompt: Option<&str>) -> Result<String>;
}

/// Descreve imagens usando Ollama (modelo vision como llava).
/// Envia a imagem em base64 via HTTP POST para /api/generate.
pub struct OllamaVisionDescriber {
    host: String,
    port: u16,
    model: String,
    timeout_secs: u64,
}

impl OllamaVisionDescriber {
    pub fn new(model: impl Into<String>) -> Self {
        let timeout_secs = std::env::var("ARREIO_MEDIA_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        Self {
            host: "localhost".into(),
            port: 11434,
            model: model.into(),
            timeout_secs,
        }
    }

    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

impl ImageDescriber for OllamaVisionDescriber {
    fn describe(&self, image_path: &Path, prompt: Option<&str>) -> Result<String> {
        let image_bytes = std::fs::read(image_path)
            .with_context(|| format!("lendo imagem {}", image_path.display()))?;
        let base64_image = base64_encode(&image_bytes);

        let user_prompt = prompt.unwrap_or("Describe this image in detail.");
        let body = serde_json::json!({
            "model": self.model,
            "prompt": user_prompt,
            "images": [base64_image],
            "stream": false
        });

        let request = format!(
            "POST /api/generate HTTP/1.1\r\n\
             Host: {}:{}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            self.host,
            self.port,
            body.to_string().len(),
            body
        );

        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .with_context(|| format!("conectando a Ollama em {}:{}", self.host, self.port))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(self.timeout_secs)))?;

        stream
            .write_all(request.as_bytes())
            .context("enviando request HTTP")?;

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .context("lendo resposta HTTP")?;

        let text = String::from_utf8_lossy(&raw);
        let body_start = text
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .or_else(|| text.find("\n\n").map(|i| i + 2))
            .unwrap_or(0);

        let resp_body = &text[body_start..];

        // Ollama responde com {"response": "...", "done": true}
        let parsed: Value = serde_json::from_str(resp_body).with_context(|| {
            format!(
                "parseando resposta Ollama: {}",
                resp_body.chars().take(200).collect::<String>()
            )
        })?;

        parsed
            .get("response")
            .and_then(|v| v.as_str().map(String::from))
            .context("campo 'response' ausente na resposta Ollama")
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = match chunk.len() {
            1 => [(chunk[0] & 0xfc) >> 2, (chunk[0] & 0x03) << 4, 0, 0],
            2 => [
                (chunk[0] & 0xfc) >> 2,
                ((chunk[0] & 0x03) << 4) | ((chunk[1] & 0xf0) >> 4),
                (chunk[1] & 0x0f) << 2,
                0,
            ],
            3 => [
                (chunk[0] & 0xfc) >> 2,
                ((chunk[0] & 0x03) << 4) | ((chunk[1] & 0xf0) >> 4),
                ((chunk[1] & 0x0f) << 2) | ((chunk[2] & 0xc0) >> 6),
                chunk[2] & 0x3f,
            ],
            _ => unreachable!(),
        };
        out.push(ALPHABET[b[0] as usize] as char);
        out.push(ALPHABET[b[1] as usize] as char);
        out.push(if chunk.len() >= 2 {
            ALPHABET[b[2] as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() >= 3 {
            ALPHABET[b[3] as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_hello() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn base64_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn vision_describer_with_timeout_secs_override() {
        let describer = OllamaVisionDescriber::new("llava")
            .with_timeout_secs(60);
        assert_eq!(describer.timeout_secs, 60);
    }

    #[test]
    fn vision_describer_reads_timeout_from_env() {
        // Guarda valor anterior para restaurar
        let prev = std::env::var("ARREIO_MEDIA_TIMEOUT_SECS").ok();
        std::env::set_var("ARREIO_MEDIA_TIMEOUT_SECS", "45");
        let describer = OllamaVisionDescriber::new("llava");
        assert_eq!(describer.timeout_secs, 45);
        // Restaura
        match prev {
            Some(v) => std::env::set_var("ARREIO_MEDIA_TIMEOUT_SECS", v),
            None => std::env::remove_var("ARREIO_MEDIA_TIMEOUT_SECS"),
        }
    }
}
