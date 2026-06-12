use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Sample de batch — um prompt individual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSample {
    pub id: String,
    pub prompt: String,
    pub metadata: Option<Value>,
}

/// Estado de checkpoint do batch runner.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BatchCheckpoint {
    pub completed_ids: Vec<String>,
    pub failed_ids: Vec<String>,
    pub total: usize,
}

/// Batch runner com checkpointing incremental.
pub struct BatchRunner {
    samples: Vec<BatchSample>,
    checkpoint_path: PathBuf,
    checkpoint: BatchCheckpoint,
}

impl BatchRunner {
    pub fn new(dataset_path: &Path, checkpoint_path: &Path) -> Result<Self> {
        let samples = Self::load_dataset(dataset_path)?;
        let checkpoint = if checkpoint_path.exists() {
            let raw = fs::read_to_string(checkpoint_path)?;
            match serde_json::from_str(&raw) {
                Ok(cp) => cp,
                Err(e) => {
                    eprintln!("[batch_runner] ERRO: checkpoint corrompido em {} — reiniciando. erro: {}", checkpoint_path.display(), e);
                    BatchCheckpoint {
                        total: samples.len(),
                        ..Default::default()
                    }
                }
            }
        } else {
            BatchCheckpoint {
                total: samples.len(),
                ..Default::default()
            }
        };
        Ok(Self {
            samples,
            checkpoint_path: checkpoint_path.to_path_buf(),
            checkpoint,
        })
    }

    fn load_dataset(path: &Path) -> Result<Vec<BatchSample>> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("lendo dataset {}", path.display()))?;
        let mut samples = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut value: Value = serde_json::from_str(line)
                .with_context(|| format!("parse JSONL linha {}", i + 1))?;
            let prompt = value
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let id = value
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(&format!("sample-{}", i))
                .to_string();
            // Remove prompt e id do metadata
            if let Value::Object(ref mut map) = value {
                map.remove("prompt");
                map.remove("id");
            }
            let metadata =
                if value.is_null() || value.as_object().map(|m| m.is_empty()).unwrap_or(true) {
                    None
                } else {
                    Some(value)
                };
            samples.push(BatchSample {
                id,
                prompt,
                metadata,
            });
        }
        Ok(samples)
    }

    /// Retorna samples pendentes (não completados e não falhados).
    pub fn pending_samples(&self) -> Vec<&BatchSample> {
        let done: std::collections::HashSet<_> =
            self.checkpoint.completed_ids.iter().cloned().collect();
        let failed: std::collections::HashSet<_> =
            self.checkpoint.failed_ids.iter().cloned().collect();
        self.samples
            .iter()
            .filter(|s| !done.contains(&s.id) && !failed.contains(&s.id))
            .collect()
    }

    /// Marca um sample como completado.
    pub fn mark_completed(&mut self, id: &str) -> Result<()> {
        if !self.checkpoint.completed_ids.contains(&id.to_string()) {
            self.checkpoint.completed_ids.push(id.to_string());
        }
        self.save_checkpoint()
    }

    /// Marca um sample como falhado.
    pub fn mark_failed(&mut self, id: &str) -> Result<()> {
        if !self.checkpoint.failed_ids.contains(&id.to_string()) {
            self.checkpoint.failed_ids.push(id.to_string());
        }
        self.save_checkpoint()
    }

    /// Verifica se há samples sem reasoning (filtro de qualidade).
    pub fn has_reasoning(sample: &BatchSample) -> bool {
        let prompt = sample.prompt.to_lowercase();
        prompt.contains("reason")
            || prompt.contains("think")
            || prompt.contains("explain")
            || prompt.contains("step")
    }

    /// Progresso atual (0.0 - 1.0).
    pub fn progress(&self) -> f64 {
        if self.checkpoint.total == 0 {
            return 1.0;
        }
        let done = self.checkpoint.completed_ids.len() + self.checkpoint.failed_ids.len();
        done as f64 / self.checkpoint.total as f64
    }

    /// Estatísticas do batch.
    pub fn stats(&self) -> BatchStats {
        BatchStats {
            total: self.checkpoint.total,
            completed: self.checkpoint.completed_ids.len(),
            failed: self.checkpoint.failed_ids.len(),
            pending: self.pending_samples().len(),
            progress: self.progress(),
        }
    }

    fn save_checkpoint(&self) -> Result<()> {
        let tmp = self.checkpoint_path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&self.checkpoint)?)?;
        fs::rename(&tmp, &self.checkpoint_path)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BatchStats {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub pending: usize,
    pub progress: f64,
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_dataset() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("dataset.jsonl");
        fs::write(
            &path,
            r#"
{"id": "s1", "prompt": "Write a function"}
{"id": "s2", "prompt": "Explain reasoning"}
{"id": "s3", "prompt": "Fix bug"}
"#,
        )
        .unwrap();
        (tmp, path)
    }

    #[test]
    fn load_dataset() {
        let (_tmp, path) = make_dataset();
        let runner = BatchRunner::new(&path, &PathBuf::from("/tmp/nonexistent.json")).unwrap();
        assert_eq!(runner.samples.len(), 3);
        assert_eq!(runner.samples[0].id, "s1");
    }

    #[test]
    fn checkpoint_resume() {
        let tmp = TempDir::new().unwrap();
        let dataset = tmp.path().join("dataset.jsonl");
        fs::write(
            &dataset,
            r#"
{"id": "s1", "prompt": "a"}
{"id": "s2", "prompt": "b"}
"#,
        )
        .unwrap();
        let checkpoint = tmp.path().join("checkpoint.json");

        let mut runner = BatchRunner::new(&dataset, &checkpoint).unwrap();
        runner.mark_completed("s1").unwrap();
        drop(runner);

        // Resume
        let runner2 = BatchRunner::new(&dataset, &checkpoint).unwrap();
        let pending = runner2.pending_samples();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "s2");
    }

    #[test]
    fn quality_filter() {
        let sample = BatchSample {
            id: "s1".to_string(),
            prompt: "Explain your reasoning".to_string(),
            metadata: None,
        };
        assert!(BatchRunner::has_reasoning(&sample));

        let sample2 = BatchSample {
            id: "s2".to_string(),
            prompt: "ok".to_string(),
            metadata: None,
        };
        assert!(!BatchRunner::has_reasoning(&sample2));
    }

    #[test]
    fn progress_calculation() {
        let (_tmp, path) = make_dataset();
        let tmp = TempDir::new().unwrap();
        let checkpoint = tmp.path().join("cp.json");
        let mut runner = BatchRunner::new(&path, &checkpoint).unwrap();
        runner.mark_completed("s1").unwrap();
        runner.mark_failed("s2").unwrap();
        assert!(runner.progress() > 0.6 && runner.progress() < 0.7);
    }
}
