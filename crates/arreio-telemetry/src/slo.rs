use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};

/// Definição de SLO (Service Level Objective).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SloDefinition {
    pub name: String,
    pub description: String,
    pub target_percent: f64, // ex: 95.0 para 95%
    pub metric_name: String,
    pub window_secs: u64,
    pub budget_exhausted_action: String, // ex: "pause_new_skills"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SloStatus {
    Healthy,
    AtRisk,
    Breached,
}

/// Registro e avaliação de SLOs.
pub struct SloRegistry {
    blackboard: Blackboard,
}

impl SloRegistry {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn register(&self, slo: &SloDefinition) -> anyhow::Result<()> {
        self.blackboard
            .put_tuple("slo", &slo.name, serde_json::to_value(slo)?)
    }

    pub fn evaluate(&self, slo_name: &str, current_value: f64) -> SloStatus {
        let slo: Option<SloDefinition> = self
            .blackboard
            .get_tuple("slo", slo_name)
            .and_then(|v| serde_json::from_value(v).ok());

        if let Some(def) = slo {
            if current_value >= def.target_percent {
                SloStatus::Healthy
            } else if current_value >= def.target_percent * 0.9 {
                SloStatus::AtRisk
            } else {
                SloStatus::Breached
            }
        } else {
            SloStatus::Healthy
        }
    }

    pub fn list(&self) -> Vec<SloDefinition> {
        self.blackboard
            .search_tuples("slo", "")
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    fn temp_reg() -> SloRegistry {
        let f = NamedTempFile::new().unwrap();
        let p: PathBuf = f.path().to_path_buf();
        drop(f);
        let bb = Blackboard::open(&p).unwrap();
        SloRegistry::new(bb)
    }

    #[test]
    fn slo_healthy_atrisk_breached() {
        let reg = temp_reg();
        reg.register(&SloDefinition {
            name: "pipeline_success".into(),
            description: "Taxa de sucesso do pipeline".into(),
            target_percent: 95.0,
            metric_name: "dag.success_rate".into(),
            window_secs: 86400,
            budget_exhausted_action: "pause".into(),
        })
        .unwrap();

        assert_eq!(reg.evaluate("pipeline_success", 96.0), SloStatus::Healthy);
        assert_eq!(reg.evaluate("pipeline_success", 90.0), SloStatus::AtRisk);
        assert_eq!(reg.evaluate("pipeline_success", 80.0), SloStatus::Breached);
    }
}
