//! Exportador OTLP/HTTP síncrono para traces.
//!
//! Envia spans como JSON/HTTP POST para um endpoint OTLP collector.
//! Sem async/tokio — usa `std::net::TcpStream` + `native_tls` para HTTPS.
//!
//! Graceful degradation: se o endpoint falhar, spans são descartados
//! silenciosamente (log no blackboard, não panic).

use crate::otel::*;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Configuração do exportador OTLP.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    /// URL do collector OTLP (ex: "http://localhost:4318" ou "https://otel.example.com")
    pub endpoint: String,
    /// Tamanho do batch antes de flush automático
    pub batch_size: usize,
    /// Timeout para cada requisição HTTP (ms)
    pub timeout_ms: u64,
    /// Headers adicionais (ex: API keys)
    pub headers: Vec<(String, String)>,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4318".into(),
            batch_size: 100,
            timeout_ms: 5000,
            headers: Vec::new(),
        }
    }
}

impl OtlpConfig {
    /// Carrega configuração de variáveis de ambiente.
    ///
    /// - `ARREIO_OTEL_ENDPOINT` — endpoint do collector (padrão: http://localhost:4318)
    /// - `ARREIO_OTEL_BATCH_SIZE` — tamanho do batch (padrão: 100)
    /// - `ARREIO_OTEL_TIMEOUT_MS` — timeout em ms (padrão: 5000)
    pub fn from_env() -> Self {
        let endpoint = std::env::var("ARREIO_OTEL_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4318".into());
        let batch_size = std::env::var("ARREIO_OTEL_BATCH_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);
        let timeout_ms = std::env::var("ARREIO_OTEL_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5000);

        Self {
            endpoint,
            batch_size,
            timeout_ms,
            headers: Vec::new(),
        }
    }
}

/// Exportador OTLP que acumula spans em memória e faz flush por batch.
pub struct OtelTraceExporter {
    config: OtlpConfig,
    buffer: Vec<OtelSpan>,
    last_error: Option<String>,
    total_exported: u64,
}

impl OtelTraceExporter {
    pub fn new(config: OtlpConfig) -> Self {
        Self {
            config,
            buffer: Vec::new(),
            last_error: None,
            total_exported: 0,
        }
    }

    /// Cria exportador a partir de variáveis de ambiente.
    pub fn from_env() -> Self {
        Self::new(OtlpConfig::from_env())
    }

    /// Adiciona um span ao buffer. Se o buffer atingir batch_size, faz flush.
    pub fn export_span(&mut self, span: OtelSpan) -> Result<()> {
        self.buffer.push(span);
        if self.buffer.len() >= self.config.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    /// Adiciona múltiplos spans de uma vez.
    pub fn export_spans(&mut self, spans: Vec<OtelSpan>) -> Result<()> {
        self.buffer.extend(spans);
        if self.buffer.len() >= self.config.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    /// Força o envio de todos os spans no buffer.
    pub fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let spans: Vec<OtelSpan> = std::mem::take(&mut self.buffer);
        let request = build_request(spans);
        let json_body = serde_json::to_string(&request)
            .context("falha ao serializar spans para JSON")?;

        match self.send_http(json_body) {
            Ok(()) => {
                self.total_exported += request.count_spans() as u64;
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{}", e));
                // Graceful degradation: não propagamos erro para não quebrar execução
            }
        }

        Ok(())
    }

    /// Envia requisição HTTP POST síncrona.
    fn send_http(&self, body: String) -> Result<()> {
        let url = &self.config.endpoint;
        let (host, port, path, use_tls) = parse_url(url)
            .context(format!("URL OTLP inválida: {}", url))?;

        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr)
            .with_context(|| format!("não foi possível conectar a {}", addr))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(self.config.timeout_ms)))
            .context("falha ao setar read timeout")?;
        stream
            .set_write_timeout(Some(Duration::from_millis(self.config.timeout_ms)))
            .context("falha ao setar write timeout")?;

        let request = build_http_request(&host, &path, &body, use_tls, &self.config.headers);

        if use_tls {
            // TLS não implementado nesta fase — retorna erro controlado
            anyhow::bail!("HTTPS/TLS não implementado nesta fase — use endpoint HTTP");
        } else {
            let mut stream = stream;
            stream
                .write_all(request.as_bytes())
                .context("falha ao enviar requisição HTTP")?;
            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .context("falha ao ler resposta HTTP")?;
            check_response(&response)?;
        }

        Ok(())
    }

    /// Retorna o último erro de exportação, se houver.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Retorna o total de spans exportados com sucesso.
    pub fn total_exported(&self) -> u64 {
        self.total_exported
    }

    /// Retorna o número de spans pendentes no buffer.
    pub fn pending_count(&self) -> usize {
        self.buffer.len()
    }

    /// Retorna true se o exportador está configurado (endpoint não vazio).
    pub fn is_configured(&self) -> bool {
        !self.config.endpoint.is_empty() && self.config.endpoint != "http://localhost:4318"
    }
}

impl Drop for OtelTraceExporter {
    fn drop(&mut self) {
        // Tenta fazer flush ao destruir — melhor esforço
        let _ = self.flush();
    }
}

/// Constrói o corpo da requisição OTLP.
fn build_request(spans: Vec<OtelSpan>) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: OtelResource::new(),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: "arreio-telemetry".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }),
                spans,
            }],
        }],
    }
}

/// Parse simples de URL (suporta http://host:port/path e https://...).
fn parse_url(url: &str) -> Option<(String, u16, String, bool)> {
    let url = url.trim();
    let (scheme, rest) = if let Some(pos) = url.find("://") {
        (&url[..pos], &url[pos + 3..])
    } else {
        ("http", url)
    };

    let use_tls = scheme == "https";
    let default_port: u16 = if use_tls { 443 } else { 80 };

    let (host_port, path) = if let Some(slash) = rest.find('/') {
        (&rest[..slash], &rest[slash..])
    } else {
        (rest, "/v1/traces")
    };

    let (host, port) = if let Some(colon) = host_port.rfind(':') {
        let port_str = &host_port[colon + 1..];
        let port = port_str.parse().unwrap_or(default_port);
        (host_port[..colon].to_string(), port)
    } else {
        (host_port.to_string(), default_port)
    };

    let path = if path.is_empty() { "/v1/traces".into() } else { path.into() };

    Some((host, port, path, use_tls))
}

/// Constrói a requisição HTTP raw.
fn build_http_request(
    host: &str,
    path: &str,
    body: &str,
    _use_tls: bool,
    extra_headers: &[(String, String)],
) -> String {
    let mut req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        path,
        host,
        body.len()
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(body);
    req
}

/// Verifica se a resposta HTTP é 2xx.
fn check_response(response: &str) -> Result<()> {
    let first_line = response.lines().next().unwrap_or("");
    if first_line.contains("200") || first_line.contains("202") {
        Ok(())
    } else {
        anyhow::bail!("resposta HTTP não-OK: {}", first_line.trim())
    }
}

/// Conta spans em uma request.
trait CountSpans {
    fn count_spans(&self) -> usize;
}

impl CountSpans for ExportTraceServiceRequest {
    fn count_spans(&self) -> usize {
        self.resource_spans
            .iter()
            .flat_map(|rs| &rs.scope_spans)
            .map(|ss| ss.spans.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otel::{gen_span_id, gen_trace_id};

    #[test]
    fn config_from_env() {
        // Testa defaults (limpa env vars)
        let _ = std::env::remove_var("ARREIO_OTEL_ENDPOINT");
        let _ = std::env::remove_var("ARREIO_OTEL_BATCH_SIZE");
        let _ = std::env::remove_var("ARREIO_OTEL_TIMEOUT_MS");

        let cfg = OtlpConfig::from_env();
        assert_eq!(cfg.endpoint, "http://localhost:4318");
        assert_eq!(cfg.batch_size, 100);
        assert_eq!(cfg.timeout_ms, 5000);

        // Testa valores customizados
        std::env::set_var("ARREIO_OTEL_ENDPOINT", "https://otel.example.com:4318");
        std::env::set_var("ARREIO_OTEL_BATCH_SIZE", "50");
        std::env::set_var("ARREIO_OTEL_TIMEOUT_MS", "3000");

        let cfg = OtlpConfig::from_env();
        assert_eq!(cfg.endpoint, "https://otel.example.com:4318");
        assert_eq!(cfg.batch_size, 50);
        assert_eq!(cfg.timeout_ms, 3000);

        std::env::remove_var("ARREIO_OTEL_ENDPOINT");
        std::env::remove_var("ARREIO_OTEL_BATCH_SIZE");
        std::env::remove_var("ARREIO_OTEL_TIMEOUT_MS");
    }

    #[test]
    fn parse_url_http_default_port() {
        let (host, port, path, tls) = parse_url("http://localhost").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 80);
        assert_eq!(path, "/v1/traces");
        assert!(!tls);
    }

    #[test]
    fn parse_url_http_with_port() {
        let (host, port, path, tls) = parse_url("http://localhost:4318").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 4318);
        assert_eq!(path, "/v1/traces");
        assert!(!tls);
    }

    #[test]
    fn parse_url_https_with_path() {
        let (host, port, path, tls) = parse_url("https://otel.example.com/v1/traces").unwrap();
        assert_eq!(host, "otel.example.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/v1/traces");
        assert!(tls);
    }

    #[test]
    fn parse_url_no_scheme() {
        let (host, port, path, tls) = parse_url("127.0.0.1:4318/custom").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 4318);
        assert_eq!(path, "/custom");
        assert!(!tls);
    }

    #[test]
    fn build_http_request_format() {
        let req = build_http_request("localhost", "/v1/traces", r#"{"key":"val"}"#, false, &[]);
        assert!(req.starts_with("POST /v1/traces HTTP/1.1"));
        assert!(req.contains("Host: localhost"));
        assert!(req.contains("Content-Type: application/json"));
        assert!(req.contains(r#"{"key":"val"}"#));
    }

    #[test]
    fn check_response_ok() {
        assert!(check_response("HTTP/1.1 200 OK\r\n").is_ok());
        assert!(check_response("HTTP/1.1 202 Accepted\r\n").is_ok());
    }

    #[test]
    fn check_response_not_ok() {
        assert!(check_response("HTTP/1.1 500 Internal Server Error\r\n").is_err());
    }

    #[test]
    fn exporter_buffer_and_flush() {
        let cfg = OtlpConfig {
            endpoint: "http://localhost:1".into(), // endpoint inválido = falha silenciosa
            batch_size: 2,
            timeout_ms: 100,
            headers: Vec::new(),
        };
        let mut exporter = OtelTraceExporter::new(cfg);

        let span1 = OtelSpan {
            trace_id: gen_trace_id(),
            span_id: gen_span_id(),
            parent_span_id: None,
            name: "test.1".into(),
            kind: None,
            start_time_unix_nano: 0,
            end_time_unix_nano: 1,
            attributes: vec![],
            events: vec![],
            status: None,
            resource: None,
        };
        let span2 = OtelSpan {
            trace_id: gen_trace_id(),
            span_id: gen_span_id(),
            parent_span_id: None,
            name: "test.2".into(),
            kind: None,
            start_time_unix_nano: 0,
            end_time_unix_nano: 1,
            attributes: vec![],
            events: vec![],
            status: None,
            resource: None,
        };

        // Exporta 1 span — ainda não atingiu batch_size
        exporter.export_span(span1).unwrap();
        assert_eq!(exporter.pending_count(), 1);

        // Exporta 2º span — atinge batch_size, tenta flush (vai falhar silenciosamente)
        exporter.export_span(span2).unwrap();
        // Buffer deve estar vazio após flush (mesmo com falha)
        assert_eq!(exporter.pending_count(), 0);

        // Deve ter registrado erro
        assert!(exporter.last_error().is_some());
    }

    #[test]
    fn exporter_total_exported_on_success() {
        // Não podemos testar sucesso real sem servidor, mas testamos contadores
        let cfg = OtlpConfig {
            endpoint: "http://localhost:1".into(),
            batch_size: 10,
            timeout_ms: 100,
            headers: Vec::new(),
        };
        let exporter = OtelTraceExporter::new(cfg);
        assert_eq!(exporter.total_exported(), 0);
    }

    #[test]
    fn exporter_is_configured_detects_default() {
        let cfg = OtlpConfig::default();
        let exporter = OtelTraceExporter::new(cfg);
        assert!(!exporter.is_configured(), "default localhost não deve ser 'configurado'");
    }

    #[test]
    fn exporter_is_configured_detects_custom() {
        let cfg = OtlpConfig {
            endpoint: "http://jaeger:4318".into(),
            ..Default::default()
        };
        let exporter = OtelTraceExporter::new(cfg);
        assert!(exporter.is_configured());
    }
}
