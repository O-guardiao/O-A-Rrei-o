use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Blackboard;

// ═══════════════════════════════════════════════════════════════════════════════
// Stigmergia — Coordenação Indireta via Ambiente Compartilhado
// ═══════════════════════════════════════════════════════════════════════════════

/// Rastro deixado por um agente no ambiente compartilhado.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StigmergyTrace {
    pub agent_id: String,
    pub action: String,
    pub timestamp: u64,
    pub payload: Value,
    pub intensity: f64, // 0.0 a 1.0, decai com o tempo
}

/// Board de stigmergia que persiste traces como tuplas no Blackboard.
pub struct StigmergyBoard {
    blackboard: Blackboard,
}

impl StigmergyBoard {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    /// Deixa um rastro no ambiente compartilhado.
    pub fn leave_trace(&mut self, trace: StigmergyTrace) -> Result<()> {
        let key = format!("{}::{}", trace.agent_id, trace.action);
        let payload = serde_json::to_value(&trace)?;
        self.blackboard.put_tuple("stigmergy", &key, payload)?;
        Ok(())
    }

    /// Lê traces filtrando opcionalmente por agent_id e/ou action.
    pub fn read_traces(&self, agent_id: Option<&str>, action: Option<&str>) -> Vec<StigmergyTrace> {
        let tuples = match (agent_id, action) {
            (Some(a), Some(b)) => {
                let prefix = format!("{}::{}", a, b);
                self.blackboard.search_tuples("stigmergy", &prefix)
            }
            (Some(a), None) => {
                let prefix = format!("{}::", a);
                self.blackboard.search_tuples("stigmergy", &prefix)
            }
            (None, Some(_)) | (None, None) => self.blackboard.search_tuples("stigmergy", ""),
        };

        let mut traces: Vec<StigmergyTrace> = tuples
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value(v).ok())
            .collect();

        // Filtra por action quando não há agent_id específico (search por prefixo não cobre)
        if agent_id.is_none() && action.is_some() {
            let target = action.unwrap();
            traces.retain(|t| t.action == target);
        }

        traces
    }

    /// Lê traces cujo timestamp seja mais recente que `since_ms` milissegundos atrás.
    pub fn read_recent_traces(&self, since_ms: u64) -> Vec<StigmergyTrace> {
        let now = now_ms();
        let cutoff = now.saturating_sub(since_ms);
        self.blackboard
            .search_tuples("stigmergy", "")
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value::<StigmergyTrace>(v).ok())
            .filter(|t| t.timestamp >= cutoff)
            .collect()
    }

    /// Aplica decaimento na intensidade de todas as traces.
    pub fn decay_traces(&mut self, decay_rate: f64) -> Result<()> {
        let tuples = self.blackboard.search_tuples("stigmergy", "");
        for (key, value) in tuples {
            if let Ok(mut trace) = serde_json::from_value::<StigmergyTrace>(value) {
                trace.intensity = (trace.intensity * (1.0 - decay_rate)).clamp(0.0, 1.0);
                let payload = serde_json::to_value(&trace)?;
                self.blackboard.put_tuple("stigmergy", &key, payload)?;
            }
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Coordenação Indireta
// ═══════════════════════════════════════════════════════════════════════════════

/// Agents NÃO se comunicam diretamente; leem traces do ambiente compartilhado.
pub struct IndirectCoordination;

impl IndirectCoordination {
    /// Publica uma trace própria e retorna todas as traces percebidas no ambiente.
    pub fn coordinate_via_environment(
        board: &mut Blackboard,
        my_trace: StigmergyTrace,
    ) -> Result<Vec<StigmergyTrace>> {
        let key = format!("{}::{}", my_trace.agent_id, my_trace.action);
        let payload = serde_json::to_value(&my_trace)?;
        board.put_tuple("stigmergy", &key, payload)?;

        let perceived = board
            .search_tuples("stigmergy", "")
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_value::<StigmergyTrace>(v).ok())
            .collect();

        Ok(perceived)
    }
}

// ── Utilitários ───────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn temp_board() -> Blackboard {
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_path_buf();
        drop(f);
        Blackboard::open(&path).unwrap()
    }

    fn make_trace(agent_id: &str, action: &str, intensity: f64) -> StigmergyTrace {
        StigmergyTrace {
            agent_id: agent_id.to_string(),
            action: action.to_string(),
            timestamp: now_ms(),
            payload: serde_json::json!({"data": action}),
            intensity,
        }
    }

    #[test]
    fn leave_and_read_trace_roundtrip() {
        let bb = temp_board();
        let mut sb = StigmergyBoard::new(bb);
        let trace = make_trace("agent_a", "build", 1.0);
        sb.leave_trace(trace.clone()).unwrap();

        let traces = sb.read_traces(None, None);
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].agent_id, "agent_a");
        assert_eq!(traces[0].action, "build");
        assert_eq!(traces[0].intensity, 1.0);
    }

    #[test]
    fn read_traces_filters_by_agent_id() {
        let bb = temp_board();
        let mut sb = StigmergyBoard::new(bb);
        sb.leave_trace(make_trace("alpha", "compile", 1.0)).unwrap();
        sb.leave_trace(make_trace("beta", "test", 1.0)).unwrap();

        let alpha_traces = sb.read_traces(Some("alpha"), None);
        assert_eq!(alpha_traces.len(), 1);
        assert_eq!(alpha_traces[0].agent_id, "alpha");

        let beta_traces = sb.read_traces(Some("beta"), None);
        assert_eq!(beta_traces.len(), 1);
        assert_eq!(beta_traces[0].agent_id, "beta");
    }

    #[test]
    fn read_traces_filters_by_action() {
        let bb = temp_board();
        let mut sb = StigmergyBoard::new(bb);
        sb.leave_trace(make_trace("agent_1", "deploy", 1.0))
            .unwrap();
        sb.leave_trace(make_trace("agent_1", "rollback", 1.0))
            .unwrap();
        sb.leave_trace(make_trace("agent_2", "deploy", 1.0))
            .unwrap();

        let deploy_traces = sb.read_traces(None, Some("deploy"));
        assert_eq!(deploy_traces.len(), 2);
        assert!(deploy_traces.iter().all(|t| t.action == "deploy"));

        let rollback_traces = sb.read_traces(None, Some("rollback"));
        assert_eq!(rollback_traces.len(), 1);
        assert_eq!(rollback_traces[0].action, "rollback");
    }

    #[test]
    fn decay_reduces_intensity() {
        let bb = temp_board();
        let mut sb = StigmergyBoard::new(bb);
        sb.leave_trace(make_trace("agent_x", "work", 1.0)).unwrap();

        sb.decay_traces(0.5).unwrap();

        let traces = sb.read_traces(None, None);
        assert_eq!(traces.len(), 1);
        assert!((traces[0].intensity - 0.5).abs() < 0.001);

        sb.decay_traces(0.5).unwrap();
        let traces = sb.read_traces(None, None);
        assert!((traces[0].intensity - 0.25).abs() < 0.001);
    }
}
