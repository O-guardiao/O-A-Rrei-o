//! Memory Consolidator — compactação e consolidação de memória no estilo OpenClaw "Dreaming".
//!
//! Fases:
//! - Light: indexa novos envelopes no GraphStore + FTS
//! - Deep: promove memórias de alta importância para ProjectMemory
//! - REM: reflexão e extração de temas (opcional, via LLM)

use anyhow::Result;
use arreio_kernel::Blackboard;

use crate::graph::Relation;
use crate::{GraphStore, MemoryEnvelope, MemoryType, ProjectMemory};

/// Política de retenção por tipo de memória.
#[derive(Debug, Clone)]
pub struct FlushPlan {
    pub episodic_days: u32,
    pub error_days: u32,
    pub semantic_permanent: bool,
    pub decision_permanent: bool,
}

impl Default for FlushPlan {
    fn default() -> Self {
        Self {
            episodic_days: 30,
            error_days: 90,
            semantic_permanent: true,
            decision_permanent: true,
        }
    }
}

/// Consolidador de memória em fases.
pub struct MemoryConsolidator {
    blackboard: Blackboard,
    plan: FlushPlan,
    threshold: usize,
}

impl MemoryConsolidator {
    pub fn new(blackboard: Blackboard) -> Self {
        Self {
            blackboard,
            plan: FlushPlan::default(),
            threshold: 500, // dispara consolidação quando > 500 envelopes
        }
    }

    pub fn with_threshold(mut self, threshold: usize) -> Self {
        self.threshold = threshold;
        self
    }

    pub fn with_plan(mut self, plan: FlushPlan) -> Self {
        self.plan = plan;
        self
    }

    /// Verifica se a memória precisa de consolidação.
    pub fn needs_consolidation(&self) -> Result<bool> {
        let count = self.envelope_count()?;
        Ok(count > self.threshold)
    }

    /// Executa consolidação completa.
    pub fn consolidate(&self, project_memory: &ProjectMemory) -> Result<ConsolidationReport> {
        let mut report = ConsolidationReport::default();

        // Light Stage: indexa tudo no GraphStore
        self.light_stage()?;
        report.light_indexed = self.envelope_count()?;

        // Deep Stage: promove memórias importantes para ProjectMemory
        self.deep_stage(project_memory, &mut report)?;

        // Flush: remove memórias antigas conforme plano
        self.flush_old(&mut report)?;

        Ok(report)
    }

    fn light_stage(&self) -> Result<()> {
        let envelopes = self.load_envelopes()?;
        let graph = GraphStore::new(self.blackboard.clone());
        for env in &envelopes {
            for tag in &env.tags {
                let _ = graph.add_relation(&Relation {
                    subject: env.id.clone(),
                    predicate: "tagged".into(),
                    object: tag.clone(),
                    confidence: env.importance,
                });
            }
            for entity in &env.entities {
                let _ = graph.add_relation(&Relation {
                    subject: env.id.clone(),
                    predicate: "mentions".into(),
                    object: entity.clone(),
                    confidence: env.confidence,
                });
            }
        }
        Ok(())
    }

    fn deep_stage(
        &self,
        project_memory: &ProjectMemory,
        report: &mut ConsolidationReport,
    ) -> Result<()> {
        let envelopes = self.load_envelopes()?;
        for env in &envelopes {
            let text = env.primary_text().unwrap_or("");
            match env.memory_type {
                MemoryType::Semantic if self.plan.semantic_permanent => {
                    let _ =
                        project_memory.append_decision(&format!("[Semantic] {}: {}", env.id, text));
                    report.deep_promoted += 1;
                }
                MemoryType::Decision if self.plan.decision_permanent => {
                    let _ =
                        project_memory.append_decision(&format!("[Decision] {}: {}", env.id, text));
                    report.deep_promoted += 1;
                }
                MemoryType::Error if env.importance >= 0.8 => {
                    let _ = project_memory
                        .append_decision(&format!("[Critical Error] {}: {}", env.id, text));
                    report.deep_promoted += 1;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn flush_old(&self, report: &mut ConsolidationReport) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let envelopes = self.load_envelopes()?;
        let mut retained = Vec::new();

        for env in &envelopes {
            let age_days = (now - env.created_at) / 86400;
            let keep = match env.memory_type {
                MemoryType::Episodic => age_days < self.plan.episodic_days as u64,
                MemoryType::Error => age_days < self.plan.error_days as u64,
                MemoryType::Semantic => self.plan.semantic_permanent,
                MemoryType::Decision => self.plan.decision_permanent,
                _ => age_days < self.plan.episodic_days as u64,
            };
            if keep {
                retained.push(env.clone());
            } else {
                report.flushed += 1;
            }
        }

        // Reescreve memórias no Blackboard
        self.blackboard
            .put_tuple("memory", "_envelopes", serde_json::to_value(&retained)?)?;
        Ok(())
    }

    fn load_envelopes(&self) -> Result<Vec<MemoryEnvelope>> {
        match self.blackboard.get_tuple("memory", "_envelopes") {
            Some(v) => Ok(serde_json::from_value(v).unwrap_or_default()),
            None => {
                // Fallback: carrega de search_tuples
                let tuples = self.blackboard.search_tuples("memory", "");
                let mut envelopes = Vec::new();
                for (_, value) in tuples {
                    if let Ok(env) = serde_json::from_value::<MemoryEnvelope>(value) {
                        envelopes.push(env);
                    }
                }
                Ok(envelopes)
            }
        }
    }

    fn envelope_count(&self) -> Result<usize> {
        Ok(self.load_envelopes()?.len())
    }
}

#[derive(Debug, Default)]
pub struct ConsolidationReport {
    pub light_indexed: usize,
    pub deep_promoted: usize,
    pub flushed: usize,
}

/// Timeline Recorder — eventos estruturados para debug/QA.
pub struct TimelineRecorder {
    blackboard: Blackboard,
}

impl TimelineRecorder {
    pub fn new(blackboard: Blackboard) -> Self {
        Self { blackboard }
    }

    pub fn record(&self, event_type: &str, data: serde_json::Value) -> Result<()> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let entry = serde_json::json!({
            "ts": ts,
            "type": event_type,
            "data": data,
        });
        // Append-style: lê lista atual, adiciona, escreve de volta
        let mut list = match self.blackboard.get_tuple("timeline", "events") {
            Some(v) => v.as_array().cloned().unwrap_or_default(),
            None => Vec::new(),
        };
        list.push(entry);
        // Limita a 1000 eventos
        if list.len() > 1000 {
            list.remove(0);
        }
        self.blackboard
            .put_tuple("timeline", "events", serde_json::json!(list))?;
        Ok(())
    }
}
