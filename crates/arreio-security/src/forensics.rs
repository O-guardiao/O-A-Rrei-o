use arreio_kernel::Blackboard;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Evidência forense de uma execução de comando.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvidence {
    pub command: String,
    pub stdout_hash: u64,
    pub stderr_hash: u64,
    pub exit_code: i32,
    pub timestamp: u64,
    pub working_dir: String,
    pub executor_hostname: String,
}

/// Coletor forense que registra hashes de saída para não-repúdio.
pub struct ExecutionForensics;

impl ExecutionForensics {
    pub fn record(
        blackboard: &Blackboard,
        command: &str,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
        work_dir: &str,
    ) {
        let evidence = ExecutionEvidence {
            command: command.into(),
            stdout_hash: hash_of(stdout),
            stderr_hash: hash_of(stderr),
            exit_code,
            timestamp: now(),
            working_dir: work_dir.into(),
            executor_hostname: hostname(),
        };
        let key = format!("{}-{}", evidence.timestamp, hash_of(command));
        let _ = blackboard.put_tuple(
            "forensics",
            &key,
            serde_json::to_value(evidence).unwrap_or_default(),
        );
    }

    /// Verifica se a evidência forense ainda é íntegra.
    pub fn verify(
        blackboard: &Blackboard,
        key: &str,
        expected_stdout: &str,
        expected_stderr: &str,
    ) -> bool {
        if let Some(val) = blackboard.get_tuple("forensics", key) {
            if let Ok(ev) = serde_json::from_value::<ExecutionEvidence>(val) {
                return hash_of(expected_stdout) == ev.stdout_hash
                    && hash_of(expected_stderr) == ev.stderr_hash;
            }
        }
        false
    }
}

fn hash_of(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}
