//! arreio-web — Cliente HTTPS síncrono usando native-tls.
//!
//! Implementação síncrona, sem async. Usa TcpStream + TlsConnector
//! para conexões HTTPS diretas.

use anyhow::{Context, Result};
use native_tls::TlsConnector;
use std::io::{Read, Write};
use std::net::TcpStream;

/// Faz GET HTTPS e retorna body (limitado a max_bytes).
pub fn https_get(url: &str, max_bytes: usize) -> Result<String> {
    let (host, port, path) = parse_url(url)?;

    let connector = TlsConnector::new().context("criando TlsConnector")?;
    let stream = TcpStream::connect((host.as_str(), port))
        .with_context(|| format!("conectando a {}:{}", host, port))?;
    let mut tls_stream = connector
        .connect(&host, stream)
        .with_context(|| format!("handshake TLS com {}", host))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: O Arreio/0.2\r\nAccept: text/html,text/plain\r\nConnection: close\r\n\r\n",
        path, host
    );

    tls_stream
        .write_all(request.as_bytes())
        .context("enviando request HTTPS GET")?;

    let mut raw = Vec::new();
    tls_stream
        .read_to_end(&mut raw)
        .context("lendo resposta HTTPS GET")?;

    let text = String::from_utf8_lossy(&raw);
    let body = extract_body(&text);
    let truncated = if body.len() > max_bytes {
        &body[..max_bytes]
    } else {
        body
    };

    Ok(truncated.to_string())
}

/// Faz POST HTTPS e retorna (status_code, body).
pub fn https_post(
    url: &str,
    body: &str,
    headers: &[(&str, &str)],
    max_bytes: usize,
) -> Result<(u16, String)> {
    let (host, port, path) = parse_url(url)?;

    let connector = TlsConnector::new().context("criando TlsConnector")?;
    let stream = TcpStream::connect((host.as_str(), port))
        .with_context(|| format!("conectando a {}:{}", host, port))?;
    let mut tls_stream = connector
        .connect(&host, stream)
        .with_context(|| format!("handshake TLS com {}", host))?;

    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: O Arreio/0.2\r\nContent-Length: {}\r\nConnection: close\r\n",
        path, host, body.len()
    );
    for (k, v) in headers {
        request.push_str(&format!("{}: {}\r\n", k, v));
    }
    request.push_str("\r\n");
    request.push_str(body);

    tls_stream
        .write_all(request.as_bytes())
        .context("enviando request HTTPS POST")?;

    let mut raw = Vec::new();
    tls_stream
        .read_to_end(&mut raw)
        .context("lendo resposta HTTPS POST")?;

    let text = String::from_utf8_lossy(&raw);
    let status = parse_status_code(&text)?;
    let body_text = extract_body(&text);
    let truncated = if body_text.len() > max_bytes {
        &body_text[..max_bytes]
    } else {
        body_text
    };

    Ok((status, truncated.to_string()))
}

/// Faz parse de uma URL https no formato "https://host:port/path?query".
/// Retorna (host, port, path). Porta padrão é 443.
fn parse_url(url: &str) -> Result<(String, u16, String)> {
    let url = url.trim();
    let (scheme, rest) = if let Some(pos) = url.find("://") {
        (&url[..pos], &url[pos + 3..])
    } else {
        anyhow::bail!("URL sem scheme: {}", url);
    };

    if scheme != "https" {
        anyhow::bail!("URL deve usar scheme https: {}", url);
    }

    let (host_port, path) = if let Some(pos) = rest.find('/') {
        (&rest[..pos], rest[pos..].to_string())
    } else {
        (rest, "/".to_string())
    };

    let (host, port) = if let Some(pos) = host_port.find(':') {
        let port_num = host_port[pos + 1..]
            .parse::<u16>()
            .with_context(|| format!("porta inválida em {}", url))?;
        (host_port[..pos].to_string(), port_num)
    } else {
        (host_port.to_string(), 443u16)
    };

    Ok((host, port, path))
}

/// Extrai o body de uma resposta HTTP procurando por \r\n\r\n ou \n\n.
fn extract_body(text: &str) -> &str {
    let body_start = text
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| text.find("\n\n").map(|i| i + 2))
        .unwrap_or(0);
    &text[body_start..]
}

/// Extrai o status code numérico da primeira linha da resposta HTTP.
fn parse_status_code(text: &str) -> Result<u16> {
    let line = text.lines().next().context("resposta HTTP vazia")?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        anyhow::bail!("linha de status HTTP inválida: {}", line);
    }
    parts[1]
        .parse::<u16>()
        .with_context(|| format!("status code inválido: {}", parts[1]))
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_https_padrao() {
        let (host, port, path) = parse_url("https://example.com/foo").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/foo");
    }

    #[test]
    fn parse_url_com_porta() {
        let (host, port, path) = parse_url("https://example.com:8443/bar?q=1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8443);
        assert_eq!(path, "/bar?q=1");
    }

    #[test]
    fn parse_url_sem_path() {
        let (host, port, path) = parse_url("https://example.com").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/");
    }

    #[test]
    fn parse_url_scheme_http_falha() {
        let err = parse_url("http://example.com/foo").unwrap_err();
        assert!(err.to_string().contains("https"));
    }

    #[test]
    fn parse_url_sem_scheme_falha() {
        let err = parse_url("example.com/foo").unwrap_err();
        assert!(err.to_string().contains("scheme"));
    }

    #[test]
    fn extract_body_com_crlf() {
        let resp = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(extract_body(resp), "hello");
    }

    #[test]
    fn extract_body_com_lf() {
        let resp = "HTTP/1.1 200 OK\nContent-Length: 5\n\nworld";
        assert_eq!(extract_body(resp), "world");
    }

    #[test]
    fn extract_body_vazio() {
        assert_eq!(extract_body(""), "");
    }

    #[test]
    fn parse_status_code_ok() {
        assert_eq!(parse_status_code("HTTP/1.1 200 OK\r\n").unwrap(), 200);
    }

    #[test]
    fn parse_status_code_not_found() {
        assert_eq!(
            parse_status_code("HTTP/1.1 404 Not Found\r\n").unwrap(),
            404
        );
    }

    #[test]
    fn parse_status_code_linha_invalida() {
        let err = parse_status_code("foobar").unwrap_err();
        assert!(err.to_string().contains("linha de status HTTP inválida"));
    }

    #[test]
    fn parse_status_code_resposta_vazia() {
        let err = parse_status_code("").unwrap_err();
        assert!(err.to_string().contains("vazia"));
    }

    #[test]
    fn parse_url_com_query_e_fragmento() {
        let (host, port, path) =
            parse_url("https://api.example.com:443/v1/search?q=rust&lang=pt#top").unwrap();
        assert_eq!(host, "api.example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/v1/search?q=rust&lang=pt#top");
    }
}
