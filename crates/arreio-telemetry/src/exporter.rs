use crate::{MetricPoint, MetricType, MetricsCollector};
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Exporter de métricas em formato OTLP-like (JSON Lines).
/// Sem dependências externas pesadas — gera JSON compatível com ingestão manual.
pub struct OtlpJsonExporter {
    collector: MetricsCollector,
    output_path: String,
}

impl OtlpJsonExporter {
    pub fn new(collector: MetricsCollector, output_path: impl Into<String>) -> Self {
        Self {
            collector,
            output_path: output_path.into(),
        }
    }

    /// Exporta todas as métricas como JSON Lines para o arquivo configurado.
    pub fn export_all(&self) -> Result<usize> {
        let points = self.collector.query("", 10000);
        let path = Path::new(&self.output_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;

        let mut count = 0;
        for pt in &points {
            let line = serde_json::to_string(&pt)?;
            writeln!(file, "{}", line)?;
            count += 1;
        }
        Ok(count)
    }

    /// Formata métricas no formato Prometheus text exposition.
    pub fn to_prometheus_text(&self) -> String {
        let points = self.collector.query("", 10000);
        let mut lines: Vec<String> = Vec::new();
        let mut grouped: std::collections::HashMap<String, Vec<&MetricPoint>> =
            std::collections::HashMap::new();

        for pt in &points {
            grouped.entry(pt.name.clone()).or_default().push(pt);
        }

        for (name, pts) in grouped {
            let metric_type = pts
                .first()
                .map(|p| p.metric_type)
                .unwrap_or(MetricType::Gauge);
            let type_str = match metric_type {
                MetricType::Counter => "counter",
                MetricType::Gauge => "gauge",
                MetricType::Histogram => "histogram",
            };
            lines.push(format!("# TYPE {} {}", name, type_str));
            for pt in pts {
                let labels = if pt.labels.is_empty() {
                    String::new()
                } else {
                    let pairs: Vec<String> = pt
                        .labels
                        .iter()
                        .map(|(k, v)| {
                            format!("{}=\"{}\"", k, v.replace('\\', "\\\\").replace('"', "\\\""))
                        })
                        .collect();
                    format!("{{{}}}", pairs.join(","))
                };
                lines.push(format!("{}{} {}", name, labels, pt.value));
            }
        }
        lines.join("\n") + "\n"
    }
}

/// Filtro de diagnósticos — seleciona eventos por severidade e categoria.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Debug,
    Info,
    Warning,
    Error,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Debug => "DEBUG",
            Severity::Info => "INFO",
            Severity::Warning => "WARNING",
            Severity::Error => "ERROR",
            Severity::Critical => "CRITICAL",
        }
    }
}

pub struct DiagnosticsFilter {
    min_severity: Severity,
    include_categories: Vec<String>,
}

impl DiagnosticsFilter {
    pub fn new(min_severity: Severity) -> Self {
        Self {
            min_severity,
            include_categories: Vec::new(),
        }
    }

    pub fn with_categories(mut self, cats: &[&str]) -> Self {
        self.include_categories = cats.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn accepts(&self, severity: Severity, category: &str) -> bool {
        let sev_ok = self.severity_rank(severity) >= self.severity_rank(self.min_severity);
        let cat_ok = self.include_categories.is_empty()
            || self.include_categories.contains(&category.to_string());
        sev_ok && cat_ok
    }

    fn severity_rank(&self, s: Severity) -> u8 {
        match s {
            Severity::Debug => 0,
            Severity::Info => 1,
            Severity::Warning => 2,
            Severity::Error => 3,
            Severity::Critical => 4,
        }
    }
}

/// Verificador de saúde de subsistemas.
pub struct HealthProbe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

impl HealthStatus {
    pub fn http_code(&self) -> u16 {
        match self {
            HealthStatus::Healthy => 200,
            HealthStatus::Degraded => 200,
            HealthStatus::Unhealthy => 503,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubsystemHealth {
    pub name: String,
    pub status: HealthStatus,
    pub message: String,
}

impl HealthProbe {
    pub fn check_all(
        blackboard_path: Option<&std::path::Path>,
        metrics_collector: Option<&MetricsCollector>,
    ) -> Vec<SubsystemHealth> {
        let mut results = Vec::new();

        // Blackboard health
        let bb_status = match blackboard_path {
            Some(p) if p.exists() => SubsystemHealth {
                name: "blackboard".into(),
                status: HealthStatus::Healthy,
                message: format!("db em {:?}", p),
            },
            Some(p) => SubsystemHealth {
                name: "blackboard".into(),
                status: HealthStatus::Unhealthy,
                message: format!("db não encontrado em {:?}", p),
            },
            None => SubsystemHealth {
                name: "blackboard".into(),
                status: HealthStatus::Degraded,
                message: "caminho não verificado".into(),
            },
        };
        results.push(bb_status);

        // Metrics health
        let metrics_status = match metrics_collector {
            Some(_c) => SubsystemHealth {
                name: "metrics".into(),
                status: HealthStatus::Healthy,
                message: "coletor ativo".into(),
            },
            None => SubsystemHealth {
                name: "metrics".into(),
                status: HealthStatus::Degraded,
                message: "coletor não configurado".into(),
            },
        };
        results.push(metrics_status);

        results
    }

    pub fn aggregate(results: &[SubsystemHealth]) -> HealthStatus {
        if results.iter().any(|r| r.status == HealthStatus::Unhealthy) {
            HealthStatus::Unhealthy
        } else if results.iter().any(|r| r.status == HealthStatus::Degraded) {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MetricsCollector;
    use arreio_kernel::Blackboard;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_collector() -> MetricsCollector {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        MetricsCollector::new(bb)
    }

    #[test]
    fn exporter_prometheus_format() {
        let col = temp_collector();
        col.count("requests_total", &[("method", "GET")]).unwrap();
        col.gauge("memory_bytes", 1024.0, &[]).unwrap();

        let exporter = OtlpJsonExporter::new(col, "/dev/null");
        let text = exporter.to_prometheus_text();
        assert!(text.contains("# TYPE requests_total counter"));
        assert!(text.contains("requests_total{method=\"GET\"} 1"));
        assert!(text.contains("# TYPE memory_bytes gauge"));
        assert!(text.contains("memory_bytes 1024"));
    }

    #[test]
    fn diagnostics_filter_basic() {
        let f = DiagnosticsFilter::new(Severity::Warning);
        assert!(f.accepts(Severity::Warning, "metrics"));
        assert!(f.accepts(Severity::Error, "metrics"));
        assert!(!f.accepts(Severity::Debug, "metrics"));
        assert!(!f.accepts(Severity::Info, "metrics"));
    }

    #[test]
    fn diagnostics_filter_categories() {
        let f = DiagnosticsFilter::new(Severity::Info).with_categories(&["metrics", "dag"]);
        assert!(f.accepts(Severity::Info, "metrics"));
        assert!(!f.accepts(Severity::Info, "memory"));
    }

    #[test]
    fn health_probe_aggregate() {
        let healthy = SubsystemHealth {
            name: "a".into(),
            status: HealthStatus::Healthy,
            message: "ok".into(),
        };
        let degraded = SubsystemHealth {
            name: "b".into(),
            status: HealthStatus::Degraded,
            message: "slow".into(),
        };
        let unhealthy = SubsystemHealth {
            name: "c".into(),
            status: HealthStatus::Unhealthy,
            message: "down".into(),
        };

        assert_eq!(
            HealthProbe::aggregate(&[healthy.clone(), healthy.clone()]),
            HealthStatus::Healthy
        );
        assert_eq!(
            HealthProbe::aggregate(&[healthy.clone(), degraded.clone()]),
            HealthStatus::Degraded
        );
        assert_eq!(
            HealthProbe::aggregate(&[healthy, degraded, unhealthy]),
            HealthStatus::Unhealthy
        );
    }
}
