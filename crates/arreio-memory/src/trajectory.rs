use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Formato ShareGPT JSONL: from/value conversations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectorySample {
    pub from: String, // "human", "gpt", "tool"
    pub value: String,
    pub metadata: Option<Value>,
}

/// Metadados de uma trajetória.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryMetadata {
    pub model: String,
    pub timestamp: u64,
    pub completed: bool,
    pub api_calls: u32,
    pub tool_stats: Vec<ToolStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStat {
    pub tool_name: String,
    pub call_count: u32,
}

/// Storage de trajetórias em formato ShareGPT JSONL.
pub struct TrajectoryStorage {
    completed_path: PathBuf,
    failed_path: PathBuf,
}

impl TrajectoryStorage {
    pub fn new(arreio_home: &Path) -> Self {
        Self {
            completed_path: arreio_home.join("trajectories/completed.jsonl"),
            failed_path: arreio_home.join("trajectories/failed.jsonl"),
        }
    }

    fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.completed_path.parent().unwrap())?;
        fs::create_dir_all(self.failed_path.parent().unwrap())?;
        Ok(())
    }

    /// Adiciona uma trajetória completada.
    pub fn append_completed(
        &self,
        samples: &[TrajectorySample],
        metadata: &TrajectoryMetadata,
    ) -> Result<()> {
        self.ensure_dirs()?;
        let entry = serde_json::json!({
            "conversations": samples,
            "metadata": metadata,
        });
        let line = serde_json::to_string(&entry)?;
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.completed_path)?;
        // Use write with append
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.completed_path)?;
        use std::io::Write;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Adiciona uma trajetória falhada.
    pub fn append_failed(
        &self,
        samples: &[TrajectorySample],
        metadata: &TrajectoryMetadata,
    ) -> Result<()> {
        self.ensure_dirs()?;
        let entry = serde_json::json!({
            "conversations": samples,
            "metadata": metadata,
        });
        let line = serde_json::to_string(&entry)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.failed_path)?;
        use std::io::Write;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Lê trajetórias completadas.
    pub fn read_completed(&self) -> Result<Vec<(Vec<TrajectorySample>, TrajectoryMetadata)>> {
        self.read_jsonl(&self.completed_path)
    }

    /// Lê trajetórias falhadas.
    pub fn read_failed(&self) -> Result<Vec<(Vec<TrajectorySample>, TrajectoryMetadata)>> {
        self.read_jsonl(&self.failed_path)
    }

    fn read_jsonl(&self, path: &Path) -> Result<Vec<(Vec<TrajectorySample>, TrajectoryMetadata)>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        let mut results = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line)?;
            let conversations: Vec<TrajectorySample> = serde_json::from_value(
                value.get("conversations")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("campo 'conversations' ausente na linha de trajetória"))?,
            )?;
            let metadata: TrajectoryMetadata = serde_json::from_value(
                value.get("metadata")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("campo 'metadata' ausente na linha de trajetória"))?,
            )?;
            results.push((conversations, metadata));
        }
        Ok(results)
    }
}

/// Compressão de trajetórias: protege primeiros e últimos turnos, comprime o meio.
pub struct TrajectoryCompressor {
    protect_first_n: usize,
    protect_last_n: usize,
    target_token_budget: usize,
}

impl TrajectoryCompressor {
    pub fn new() -> Self {
        Self {
            protect_first_n: 4,
            protect_last_n: 4,
            target_token_budget: 15250,
        }
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.target_token_budget = budget;
        self
    }

    /// Comprime uma trajetória preservando início e fim.
    pub fn compress(&self, samples: &[TrajectorySample]) -> Vec<TrajectorySample> {
        if samples.len() <= self.protect_first_n + self.protect_last_n {
            return samples.to_vec();
        }

        let mut result = Vec::new();
        // Protege primeiros N
        for i in 0..self.protect_first_n {
            result.push(samples[i].clone());
        }

        // Comprime o meio em um summary
        let middle_start = self.protect_first_n;
        let middle_end = samples.len() - self.protect_last_n;
        let summary = self.summarize_middle(&samples[middle_start..middle_end]);
        result.push(TrajectorySample {
            from: "summary".to_string(),
            value: summary,
            metadata: None,
        });

        // Protege últimos N
        for i in middle_end..samples.len() {
            result.push(samples[i].clone());
        }

        result
    }

    fn summarize_middle(&self, samples: &[TrajectorySample]) -> String {
        let count = samples.len();
        let tools_used: Vec<String> = samples
            .iter()
            .filter(|s| s.from == "tool")
            .map(|s| s.value.chars().take(50).collect())
            .collect();

        format!(
            "[Compressed {} turns. Tools used: {}]",
            count,
            if tools_used.is_empty() {
                "none".to_string()
            } else {
                tools_used.join(", ")
            }
        )
    }
}

impl Default for TrajectoryCompressor {
    fn default() -> Self {
        Self::new()
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_sample(from: &str, value: &str) -> TrajectorySample {
        TrajectorySample {
            from: from.to_string(),
            value: value.to_string(),
            metadata: None,
        }
    }

    #[test]
    fn storage_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let storage = TrajectoryStorage::new(tmp.path());
        let samples = vec![make_sample("human", "hello"), make_sample("gpt", "hi")];
        let meta = TrajectoryMetadata {
            model: "gemma4".to_string(),
            timestamp: 1234567890,
            completed: true,
            api_calls: 2,
            tool_stats: vec![],
        };
        storage.append_completed(&samples, &meta).unwrap();

        let read = storage.read_completed().unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0.len(), 2);
        assert_eq!(read[0].1.model, "gemma4");
    }

    #[test]
    fn compressor_protects_head_and_tail() {
        let compressor = TrajectoryCompressor::new().with_budget(1000);
        let samples: Vec<TrajectorySample> = (0..10)
            .map(|i| make_sample(&format!("from{}", i), &format!("value{}", i)))
            .collect();

        let compressed = compressor.compress(&samples);
        assert_eq!(compressed.len(), 9); // 4 + 1 summary + 4
        assert_eq!(compressed[0].from, "from0");
        assert_eq!(compressed[8].from, "from9");
        assert_eq!(compressed[4].from, "summary");
    }

    #[test]
    fn compressor_short_trajectory_unchanged() {
        let compressor = TrajectoryCompressor::new();
        let samples = vec![make_sample("human", "a"), make_sample("gpt", "b")];
        let compressed = compressor.compress(&samples);
        assert_eq!(compressed.len(), 2);
    }

    #[test]
    fn empty_storage() {
        let tmp = TempDir::new().unwrap();
        let storage = TrajectoryStorage::new(tmp.path());
        let read = storage.read_completed().unwrap();
        assert!(read.is_empty());
    }
}
