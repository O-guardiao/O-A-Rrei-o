//! Integração entre TrajectoryStore e OtelTraceExporter.
//!
//! Fornece um wrapper que registra trajetórias no Blackboard e
//! opcionalmente exporta spans OTLP.

use crate::converter::trajectory_to_otel;
use crate::exporter_otlp::OtelTraceExporter;
use anyhow::Result;
use arreio_kernel::{TrajectoryEntry, TrajectoryStore};

/// Wrapper que integra TrajectoryStore com exportação OTLP.
pub struct TelemetryBridge {
    store: TrajectoryStore,
    exporter: Option<OtelTraceExporter>,
}

impl TelemetryBridge {
    pub fn new(store: TrajectoryStore) -> Self {
        Self {
            store,
            exporter: None,
        }
    }

    /// Ativa exportação OTLP a partir de variáveis de ambiente.
    pub fn with_otlp_from_env(mut self) -> Self {
        let exporter = OtelTraceExporter::from_env();
        if exporter.is_configured() {
            self.exporter = Some(exporter);
        }
        self
    }

    /// Ativa exportação OTLP com configuração explícita.
    pub fn with_otlp(mut self, exporter: OtelTraceExporter) -> Self {
        self.exporter = Some(exporter);
        self
    }

    /// Registra uma trajetória e exporta span OTLP se configurado.
    pub fn record(&mut self, entry: &TrajectoryEntry) -> Result<()> {
        self.store.record(entry)?;

        if let Some(exporter) = &mut self.exporter {
            let span = trajectory_to_otel(entry);
            // Graceful degradation: falha no export não quebra o record
            let _ = exporter.export_span(span);
        }

        Ok(())
    }

    /// Força flush do exportador OTLP.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(exporter) = &mut self.exporter {
            exporter.flush()?;
        }
        Ok(())
    }

    /// Retorna referência ao TrajectoryStore interno.
    pub fn store(&self) -> &TrajectoryStore {
        &self.store
    }

    /// Retorna true se exportação OTLP está ativa.
    pub fn otlp_enabled(&self) -> bool {
        self.exporter.is_some()
    }

    /// Retorna o total de spans exportados.
    pub fn total_exported(&self) -> u64 {
        self.exporter.as_ref().map_or(0, |e| e.total_exported())
    }

    /// Retorna o último erro do exportador, se houver.
    pub fn last_error(&self) -> Option<&str> {
        self.exporter.as_ref().and_then(|e| e.last_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreio_kernel::{HitlStatus, TrajectoryResult};
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_bridge() -> TelemetryBridge {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = arreio_kernel::Blackboard::open(&p).unwrap();
        let store = TrajectoryStore::new(bb);
        TelemetryBridge::new(store)
    }

    fn make_entry(task_id: &str) -> TrajectoryEntry {
        TrajectoryEntry {
            task_id: task_id.into(),
            timestamp: 1_717_000_000,
            specification: "test".into(),
            contract: None,
            generated_code_snippet: None,
            code_hash: None,
            validation_cmd: None,
            result: TrajectoryResult::Success {
                test_count: 1,
                test_passed: 1,
            },
            models_used: vec![],
            tokens_consumed: 100,
            duration_ms: 1000,
            attempt_number: 1,
            contract_violations: vec![],
            hitl_status: HitlStatus::NotApplicable,
            human_decision: None,
        }
    }

    #[test]
    fn record_without_otlp_works() {
        let mut bridge = temp_bridge();
        let entry = make_entry("task_001");
        bridge.record(&entry).unwrap();
        assert!(!bridge.otlp_enabled());
        assert_eq!(bridge.total_exported(), 0);
    }

    #[test]
    fn record_with_otlp_buffering() {
        let mut bridge = temp_bridge();
        // Configura exportador com endpoint inválido (para não tentar conectar)
        let cfg = crate::exporter_otlp::OtlpConfig {
            endpoint: "http://localhost:1".into(),
            batch_size: 10,
            timeout_ms: 100,
            headers: vec![],
        };
        let exporter = OtelTraceExporter::new(cfg);
        bridge = bridge.with_otlp(exporter);

        assert!(bridge.otlp_enabled());

        let entry = make_entry("task_002");
        bridge.record(&entry).unwrap();

        // Span fica no buffer, não foi exportado ainda (batch não atingido)
        assert_eq!(bridge.total_exported(), 0);
    }

    #[test]
    fn otlp_disabled_when_endpoint_is_default() {
        let bridge = temp_bridge().with_otlp_from_env();
        // Sem ARREIO_OTEL_ENDPOINT set, is_configured() retorna false
        assert!(!bridge.otlp_enabled());
    }
}
