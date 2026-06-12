use anyhow::{Context, Result};
use native_tls::{TlsConnector, TlsStream};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Cliente TLS nativo para requisições HTTPS síncronas.
///
/// Usa `native-tls` para o handshake TLS e `std::net::TcpStream` para o transporte.
/// Toda a API é bloqueante (sem async), alinhada com o restante do Arreio.
pub struct TlsClient;

impl TlsClient {
    /// Conecta a `host:port` via TLS e retorna o stream criptografado.
    pub fn connect(host: &str, port: u16) -> Result<TlsStream<TcpStream>> {
        let connector = TlsConnector::new().context("falha ao criar TlsConnector")?;
        let stream = TcpStream::connect((host, port))
            .with_context(|| format!("falha ao conectar em {}:{}", host, port))?;
        let tls_stream = connector
            .connect(host, stream)
            .with_context(|| format!("falha no handshake TLS com {}", host))?;
        Ok(tls_stream)
    }

    /// Faz POST HTTPS e retorna `(status_code, response_body)`.
    pub fn https_post(
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<(u16, String)> {
        let mut stream = Self::connect(host, port)?;
        let request = build_post_request(host, path, headers, body);
        stream
            .write_all(request.as_bytes())
            .context("falha ao enviar request POST")?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .context("falha ao ler resposta POST")?;
        parse_status_and_body(&response)
    }

    /// Faz GET HTTPS e retorna `(status_code, response_body)`.
    pub fn https_get(
        host: &str,
        port: u16,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<(u16, String)> {
        let mut stream = Self::connect(host, port)?;
        let request = build_get_request(host, path, headers);
        stream
            .write_all(request.as_bytes())
            .context("falha ao enviar request GET")?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .context("falha ao ler resposta GET")?;
        parse_status_and_body(&response)
    }
}

/// Monta o payload textual de uma requisição HTTP/1.1 POST.
pub(crate) fn build_post_request(
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> String {
    let headers_str = headers
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v))
        .collect::<Vec<_>>()
        .join("\r\n");
    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        path,
        host,
        body.len()
    );
    if !headers_str.is_empty() {
        request.push_str(&headers_str);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);
    request
}

/// Monta o payload textual de uma requisição HTTP/1.1 GET.
pub(crate) fn build_get_request(host: &str, path: &str, headers: &[(&str, &str)]) -> String {
    let headers_str = headers
        .iter()
        .map(|(k, v)| format!("{}: {}", k, v))
        .collect::<Vec<_>>()
        .join("\r\n");
    let mut request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        path, host
    );
    if !headers_str.is_empty() {
        request.push_str(&headers_str);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request
}

/// Extrai o código de status e o corpo de uma resposta HTTP/1.x bruta.
pub(crate) fn parse_status_and_body(response: &str) -> Result<(u16, String)> {
    let status = response
        .lines()
        .next()
        .context("resposta HTTP vazia")?
        .split_whitespace()
        .nth(1)
        .context("status HTTP ausente")?
        .parse::<u16>()
        .context("status HTTP inválido")?;
    let body_start = response.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    Ok((status, response[body_start..].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_post_request_sem_headers() {
        let req = build_post_request("api.exemplo.com", "/v1/chat", &[], r#"{"msg":"ola"}"#);
        assert!(req.starts_with("POST /v1/chat HTTP/1.1\r\n"));
        assert!(req.contains("Host: api.exemplo.com\r\n"));
        assert!(req.contains("Content-Length: 13\r\n"));
        assert!(req.ends_with("\r\n\r\n{\"msg\":\"ola\"}"));
    }

    #[test]
    fn build_post_request_com_headers() {
        let req = build_post_request(
            "api.exemplo.com",
            "/v1/chat",
            &[
                ("Authorization", "Bearer tok"),
                ("Content-Type", "application/json"),
            ],
            "body",
        );
        assert!(req.contains("Authorization: Bearer tok\r\n"));
        assert!(req.contains("Content-Type: application/json\r\n"));
        let parts: Vec<&str> = req.split("\r\n").collect();
        assert!(parts.contains(&"Authorization: Bearer tok"));
        assert!(parts.contains(&"Content-Type: application/json"));
    }

    #[test]
    fn build_get_request_sem_headers() {
        let req = build_get_request("api.exemplo.com", "/v1/models", &[]);
        assert!(req.starts_with("GET /v1/models HTTP/1.1\r\n"));
        assert!(req.contains("Host: api.exemplo.com\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn build_get_request_com_headers() {
        let req = build_get_request("api.exemplo.com", "/v1/models", &[("X-Api-Key", "123")]);
        assert!(req.contains("X-Api-Key: 123\r\n"));
    }

    #[test]
    fn parse_status_and_body_ok() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        let (status, body) = parse_status_and_body(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"ok":true}"#);
    }

    #[test]
    fn parse_status_and_body_404() {
        let raw = "HTTP/1.1 404 Not Found\r\n\r\n";
        let (status, body) = parse_status_and_body(raw).unwrap();
        assert_eq!(status, 404);
        assert_eq!(body, "");
    }

    #[test]
    fn parse_status_and_body_invalido() {
        let raw = "HTTP/1.1 OK\r\n\r\n";
        assert!(parse_status_and_body(raw).is_err());
    }

    #[test]
    fn connect_host_invalido_falha() {
        // Porta 1 em localhost é extremamente improvável de aceitar conexão.
        let result = TlsClient::connect("127.0.0.1", 1);
        assert!(result.is_err());
    }

    #[test]
    fn https_post_host_invalido_falha() {
        let result = TlsClient::https_post("127.0.0.1", 1, "/", &[], "{}");
        assert!(result.is_err());
    }

    #[test]
    fn https_get_host_invalido_falha() {
        let result = TlsClient::https_get("127.0.0.1", 1, "/", &[]);
        assert!(result.is_err());
    }
}
