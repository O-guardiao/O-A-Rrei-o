//! Modelos OTLP (OpenTelemetry Protocol) para exportação de traces.
//!
//! Implementação própria — sem depender do crate `opentelemetry` oficial.
//! Formato: JSON/HTTP conforme especificação OTLP.
//!
//! Referência: https://opentelemetry.io/docs/specs/otlp/

use serde::{Deserialize, Serialize};

/// Identificador de trace (16 bytes hex = 32 chars).
pub type TraceId = String;

/// Identificador de span (8 bytes hex = 16 chars).
pub type SpanId = String;

/// Um span OTLP representa uma operação unitária no sistema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtelSpan {
    /// trace_id em hex (32 chars)
    pub trace_id: TraceId,
    /// span_id em hex (16 chars)
    pub span_id: SpanId,
    /// parent_span_id em hex (16 chars), opcional
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    /// Nome da operação (ex: "dag.execute", "hitl.decision")
    pub name: String,
    /// Tipo de span: internal, server, client, producer, consumer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<SpanKind>,
    /// Timestamp de início (nanos desde UNIX_EPOCH)
    pub start_time_unix_nano: u64,
    /// Timestamp de fim (nanos desde UNIX_EPOCH)
    pub end_time_unix_nano: u64,
    /// Atributos chave-valor
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attributes: Vec<OtelAttribute>,
    /// Eventos (pontos no tempo dentro do span)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub events: Vec<OtelEvent>,
    /// Status do span (Unset, Ok, Error)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<OtelStatus>,
    /// Recurso associado (ex: service.name="arreio")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<OtelResource>,
}

/// Tipo de span.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpanKind {
    #[serde(rename = "SPAN_KIND_INTERNAL")]
    Internal,
    #[serde(rename = "SPAN_KIND_SERVER")]
    Server,
    #[serde(rename = "SPAN_KIND_CLIENT")]
    Client,
    #[serde(rename = "SPAN_KIND_PRODUCER")]
    Producer,
    #[serde(rename = "SPAN_KIND_CONSUMER")]
    Consumer,
}

impl Default for SpanKind {
    fn default() -> Self {
        SpanKind::Internal
    }
}

/// Atributo OTLP (chave-valor tipado).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtelAttribute {
    pub key: String,
    pub value: OtelAnyValue,
}

impl OtelAttribute {
    pub fn string(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: OtelAnyValue::String(value.into()),
        }
    }

    pub fn int(key: impl Into<String>, value: i64) -> Self {
        Self {
            key: key.into(),
            value: OtelAnyValue::Int(value),
        }
    }

    pub fn bool(key: impl Into<String>, value: bool) -> Self {
        Self {
            key: key.into(),
            value: OtelAnyValue::Bool(value),
        }
    }

    pub fn double(key: impl Into<String>, value: f64) -> Self {
        Self {
            key: key.into(),
            value: OtelAnyValue::Double(value),
        }
    }
}

/// Valor OTLP polimórfico.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OtelAnyValue {
    String(String),
    Bool(bool),
    Int(i64),
    Double(f64),
    #[serde(rename = "arrayValue")]
    Array(Vec<OtelAnyValue>),
}

/// Evento dentro de um span.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OtelEvent {
    /// Nome do evento (ex: "hitl.decision", "contract.violation")
    pub name: String,
    /// Timestamp do evento (nanos desde UNIX_EPOCH)
    pub time_unix_nano: u64,
    /// Atributos do evento
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attributes: Vec<OtelAttribute>,
}

impl OtelEvent {
    pub fn new(name: impl Into<String>, time_unix_nano: u64) -> Self {
        Self {
            name: name.into(),
            time_unix_nano,
            attributes: Vec::new(),
        }
    }

    pub fn with_attribute(mut self, attr: OtelAttribute) -> Self {
        self.attributes.push(attr);
        self
    }
}

/// Status do span.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtelStatus {
    pub code: StatusCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StatusCode {
    #[serde(rename = "STATUS_CODE_UNSET")]
    Unset,
    #[serde(rename = "STATUS_CODE_OK")]
    Ok,
    #[serde(rename = "STATUS_CODE_ERROR")]
    Error,
}

impl Default for StatusCode {
    fn default() -> Self {
        StatusCode::Unset
    }
}

/// Recurso OTLP (metadados do serviço).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OtelResource {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attributes: Vec<OtelAttribute>,
}

impl OtelResource {
    pub fn new() -> Self {
        Self {
            attributes: vec![
                OtelAttribute::string("service.name", "arreio"),
                OtelAttribute::string("service.version", env!("CARGO_PKG_VERSION")),
            ],
        }
    }

    pub fn with_attribute(mut self, attr: OtelAttribute) -> Self {
        self.attributes.push(attr);
        self
    }
}

/// Request body OTLP/JSON para exportação de traces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportTraceServiceRequest {
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpans {
    pub resource: OtelResource,
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpans {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<InstrumentationScope>,
    pub spans: Vec<OtelSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentationScope {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Gera um trace_id aleatório (32 hex chars).
pub fn gen_trace_id() -> TraceId {
    format!("{:032x}", rand::random::<u128>())
}

/// Gera um span_id aleatório (16 hex chars).
pub fn gen_span_id() -> SpanId {
    format!("{:016x}", rand::random::<u64>())
}

/// Converte timestamp em segundos para nanos.
pub fn secs_to_nanos(secs: u64) -> u64 {
    secs.saturating_mul(1_000_000_000)
}

/// Converte timestamp em milissegundos para nanos.
pub fn millis_to_nanos(millis: u64) -> u64 {
    millis.saturating_mul(1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otel_span_serializes_to_json() {
        let span = OtelSpan {
            trace_id: gen_trace_id(),
            span_id: gen_span_id(),
            parent_span_id: None,
            name: "dag.execute".into(),
            kind: Some(SpanKind::Internal),
            start_time_unix_nano: 1_000_000_000,
            end_time_unix_nano: 2_000_000_000,
            attributes: vec![
                OtelAttribute::string("task_id", "task_001"),
                OtelAttribute::int("attempt", 1),
            ],
            events: vec![OtelEvent::new("contract.check", 1_500_000_000)],
            status: Some(OtelStatus {
                code: StatusCode::Ok,
                message: None,
            }),
            resource: None,
        };

        let json = serde_json::to_string(&span).unwrap();
        assert!(json.contains("dag.execute"));
        assert!(json.contains("task_id"));
        assert!(json.contains("SPAN_KIND_INTERNAL"));
        assert!(json.contains("STATUS_CODE_OK"));
    }

    #[test]
    fn otel_event_with_attributes() {
        let ev = OtelEvent::new("hitl.decision", 1_000_000_000)
            .with_attribute(OtelAttribute::string("decision", "Approved"))
            .with_attribute(OtelAttribute::string("approver", "admin"));

        assert_eq!(ev.name, "hitl.decision");
        assert_eq!(ev.attributes.len(), 2);
    }

    #[test]
    fn export_request_serializes() {
        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: OtelResource::new(),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: "arreio-telemetry".into(),
                        version: Some("1.0.0".into()),
                    }),
                    spans: vec![OtelSpan {
                        trace_id: gen_trace_id(),
                        span_id: gen_span_id(),
                        parent_span_id: None,
                        name: "test.span".into(),
                        kind: None,
                        start_time_unix_nano: 0,
                        end_time_unix_nano: 1,
                        attributes: vec![],
                        events: vec![],
                        status: None,
                        resource: None,
                    }],
                }],
            }],
        };

        let json = serde_json::to_string_pretty(&req).unwrap();
        assert!(json.contains("resourceSpans"));
        assert!(json.contains("arreio"));
    }

    #[test]
    fn gen_trace_id_is_32_hex_chars() {
        let tid = gen_trace_id();
        assert_eq!(tid.len(), 32);
        assert!(u128::from_str_radix(&tid, 16).is_ok());
    }

    #[test]
    fn gen_span_id_is_16_hex_chars() {
        let sid = gen_span_id();
        assert_eq!(sid.len(), 16);
        assert!(u64::from_str_radix(&sid, 16).is_ok());
    }

    #[test]
    fn secs_to_nanos_conversion() {
        assert_eq!(secs_to_nanos(1), 1_000_000_000);
        assert_eq!(secs_to_nanos(0), 0);
    }

    #[test]
    fn millis_to_nanos_conversion() {
        assert_eq!(millis_to_nanos(1), 1_000_000);
        assert_eq!(millis_to_nanos(1000), 1_000_000_000);
    }
}
