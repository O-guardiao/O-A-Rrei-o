use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

// ── Shadow Git Store ──────────────────────────────────────────────────────────

/// Repositório git bare dedicado para checkpoints, isolado por projeto.
/// Layout: `.arreio/checkpoints/store/` (bare repo)
/// Refs: `refs/arreio/<hash16>`
pub struct ShadowGitStore {
    store_dir: PathBuf,
}

impl ShadowGitStore {
    pub fn new(arreio_home: impl AsRef<Path>) -> Result<Self> {
        let store_dir = arreio_home.as_ref().join("checkpoints/store");
        fs::create_dir_all(&store_dir)?;

        if !store_dir.join("HEAD").exists() {
            let init = Command::new("git")
                .args(["init", "--bare"])
                .current_dir(&store_dir)
                .output()
                .context("git init --bare falhou")?;
            if !init.status.success() {
                bail!(
                    "init bare falhou: {}",
                    String::from_utf8_lossy(&init.stderr)
                );
            }
        }

        Ok(Self { store_dir })
    }

    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    /// Cria um checkpoint de um worktree.
    /// Retorna o hash curto do commit.
    pub fn checkpoint(&self, worktree: impl AsRef<Path>, project_hash: &str) -> Result<String> {
        let worktree = worktree.as_ref();
        let git_dir = &self.store_dir;
        let index_file = self.store_dir.join(format!("index.{}", project_hash));

        // Stage tudo no worktree
        let add = Command::new("git")
            .env("GIT_DIR", git_dir)
            .env("GIT_WORK_TREE", worktree)
            .env("GIT_INDEX_FILE", &index_file)
            .args(["add", "-A"])
            .output()
            .context("git add -A falhou")?;
        if !add.status.success() {
            bail!("git add falhou: {}", String::from_utf8_lossy(&add.stderr));
        }

        // Commit
        let msg = format!("arreio:ckpt:{}", project_hash);
        let commit = Command::new("git")
            .env("GIT_DIR", git_dir)
            .env("GIT_WORK_TREE", worktree)
            .env("GIT_INDEX_FILE", &index_file)
            .args([
                "-c",
                "user.email=arreio@harness",
                "-c",
                "user.name=Arreio",
                "commit",
                "-m",
                &msg,
            ])
            .output()
            .context("git commit falhou")?;

        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            if !stderr.contains("nothing to commit") {
                bail!("git commit falhou: {}", stderr);
            }
            // Nothing to commit — retorna HEAD atual
        }

        // Hash do commit
        let hash = Command::new("git")
            .env("GIT_DIR", git_dir)
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .context("git rev-parse falhou")?;
        let hash_str = String::from_utf8_lossy(&hash.stdout).trim().to_string();

        // Tag ref
        let _ = Command::new("git")
            .env("GIT_DIR", git_dir)
            .args([
                "update-ref",
                &format!("refs/arreio/{}", project_hash),
                &hash_str,
            ])
            .output();

        Ok(hash_str)
    }

    /// Rollback de um worktree para um checkpoint.
    pub fn rollback(&self, worktree: impl AsRef<Path>, checkpoint_hash: &str) -> Result<()> {
        let worktree = worktree.as_ref();
        let git_dir = &self.store_dir;

        // Verifica se o hash existe
        let verify = Command::new("git")
            .env("GIT_DIR", git_dir)
            .args(["cat-file", "-t", checkpoint_hash])
            .output()
            .context("git cat-file falhou")?;
        if !verify.status.success() {
            bail!("checkpoint hash inválido: {}", checkpoint_hash);
        }

        // Reset hard no worktree
        let reset = Command::new("git")
            .env("GIT_DIR", git_dir)
            .env("GIT_WORK_TREE", worktree)
            .args(["reset", "--hard", checkpoint_hash])
            .output()
            .context("git reset falhou")?;
        if !reset.status.success() {
            bail!(
                "rollback falhou: {}",
                String::from_utf8_lossy(&reset.stderr)
            );
        }
        Ok(())
    }

    /// Lista checkpoints conhecidos (refs/arreio/*).
    pub fn list_checkpoints(&self) -> Result<Vec<(String, String)>> {
        let out = Command::new("git")
            .env("GIT_DIR", &self.store_dir)
            .args([
                "for-each-ref",
                "--format=%(refname:short) %(objectname:short)",
                "refs/arreio/",
            ])
            .output()
            .context("git for-each-ref falhou")?;

        let mut checkpoints = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() == 2 {
                checkpoints.push((parts[0].to_string(), parts[1].to_string()));
            }
        }
        Ok(checkpoints)
    }

    /// Remove refs órfãs cujo worktree não existe mais.
    pub fn prune_orphaned(&self, known_worktrees: &[PathBuf]) -> Result<usize> {
        let known_names: std::collections::HashSet<String> = known_worktrees
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();

        let checkpoints = self.list_checkpoints()?;
        let mut pruned = 0;
        for (ref_name, _) in checkpoints {
            let project_name = ref_name.strip_prefix("refs/arreio/").unwrap_or(&ref_name);
            if !known_names.contains(project_name) {
                let _ = Command::new("git")
                    .env("GIT_DIR", &self.store_dir)
                    .args(["update-ref", "-d", &ref_name])
                    .output();
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Tamanho total do store em bytes.
    pub fn total_size_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        for entry in walkdir(&self.store_dir)? {
            if let Ok(meta) = fs::metadata(&entry) {
                total += meta.len();
            }
        }
        Ok(total)
    }
}

fn walkdir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    result.push(path);
                }
            }
        }
    }
    Ok(result)
}

// ── Checkpoint Manager ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub hash: String,
    pub project_hash: String,
    pub worktree: PathBuf,
    pub created_at: u64,
}

/// Gerencia checkpoints com políticas de retenção.
pub struct CheckpointManager {
    store: ShadowGitStore,
    records: HashMap<String, CheckpointRecord>,
    max_total_size_mb: u64,
    retention_days: u32,
    records_path: PathBuf,
}

impl CheckpointManager {
    pub fn new(arreio_home: impl AsRef<Path>) -> Result<Self> {
        let arreio_home = arreio_home.as_ref();
        let store = ShadowGitStore::new(arreio_home)?;
        let records_path = arreio_home.join("checkpoints/records.json");
        let records = if records_path.exists() {
            let raw = fs::read_to_string(&records_path)?;
            match serde_json::from_str(&raw) {
                Ok(map) => map,
                Err(e) => {
                    eprintln!("[shadow_git] ERRO: records.json corrompido em {} — iniciando vazio. erro: {}", records_path.display(), e);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };
        Ok(Self {
            store,
            records,
            max_total_size_mb: 1024, // 1GB default
            retention_days: 30,
            records_path,
        })
    }

    pub fn with_limits(mut self, max_size_mb: u64, retention_days: u32) -> Self {
        self.max_total_size_mb = max_size_mb;
        self.retention_days = retention_days;
        self
    }

    /// Cria checkpoint se necessário (máx 1 por turno, antes de operações mutadoras).
    pub fn maybe_checkpoint(
        &mut self,
        worktree: impl AsRef<Path>,
        project_hash: &str,
        is_mutating: bool,
    ) -> Result<Option<String>> {
        if !is_mutating {
            return Ok(None);
        }

        let hash = self.store.checkpoint(&worktree, project_hash)?;
        let record = CheckpointRecord {
            hash: hash.clone(),
            project_hash: project_hash.to_string(),
            worktree: worktree.as_ref().to_path_buf(),
            created_at: now_epoch_secs(),
        };
        self.records.insert(hash.clone(), record);
        self.save_records()?;
        Ok(Some(hash))
    }

    /// Rollback para o checkpoint mais recente de um projeto.
    pub fn rollback_latest(
        &self,
        worktree: impl AsRef<Path>,
        project_hash: &str,
    ) -> Result<String> {
        let latest = self
            .records
            .values()
            .filter(|r| r.project_hash == project_hash)
            .max_by_key(|r| r.created_at)
            .map(|r| r.hash.clone())
            .ok_or_else(|| anyhow::anyhow!("nenhum checkpoint encontrado para {}", project_hash))?;
        self.store.rollback(&worktree, &latest)?;
        Ok(latest)
    }

    /// Rollback para hash específico.
    pub fn rollback_hash(&self, worktree: impl AsRef<Path>, hash: &str) -> Result<()> {
        self.store.rollback(&worktree, hash)
    }

    /// Auto-prune: remove órfãos e stale checkpoints.
    pub fn maybe_auto_prune(&mut self, known_worktrees: &[PathBuf]) -> Result<PruneReport> {
        let mut report = PruneReport::default();

        // Prune refs órfãs
        report.orphaned_refs = self.store.prune_orphaned(known_worktrees)?;

        // Prune records stale
        let now = now_epoch_secs();
        let cutoff = now - (self.retention_days as u64 * 86400);
        let stale_hashes: Vec<String> = self
            .records
            .values()
            .filter(|r| r.created_at < cutoff)
            .map(|r| r.hash.clone())
            .collect();
        for hash in stale_hashes {
            self.records.remove(&hash);
            report.stale_records += 1;
        }

        // Prune por tamanho total
        let size_mb = self.store.total_size_bytes()? / (1024 * 1024);
        if size_mb > self.max_total_size_mb {
            // Ordena por data, remove os mais antigos
            let mut sorted: Vec<_> = self.records.values().cloned().collect();
            sorted.sort_by_key(|r| r.created_at);
            let target = self.max_total_size_mb / 2;
            while size_mb > target && !sorted.is_empty() {
                if let Some(old) = sorted.first() {
                    self.records.remove(&old.hash);
                    sorted.remove(0);
                    report.size_pruned += 1;
                }
            }
        }

        self.save_records()?;
        Ok(report)
    }

    pub fn records(&self) -> &HashMap<String, CheckpointRecord> {
        &self.records
    }

    fn save_records(&self) -> Result<()> {
        fs::create_dir_all(self.records_path.parent().unwrap())?;
        let tmp = self.records_path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&self.records)?)?;
        fs::rename(&tmp, &self.records_path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub orphaned_refs: usize,
    pub stale_records: usize,
    pub size_pruned: usize,
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn temp_arreio_home() -> PathBuf {
        TempDir::new().unwrap().path().to_path_buf()
    }

    fn init_git_worktree(path: &Path) {
        fs::create_dir_all(path).unwrap();
        let _ = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output();
        fs::write(path.join("file.txt"), "hello").unwrap();
        let _ = Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output();
        let _ = Command::new("git")
            .args([
                "-c",
                "user.email=a@b",
                "-c",
                "user.name=A",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(path)
            .output();
    }

    #[test]
    fn shadow_git_store_creates_bare_repo() {
        let home = temp_arreio_home();
        let store = ShadowGitStore::new(&home).unwrap();
        assert!(store.store_dir().join("HEAD").exists());
    }

    #[test]
    fn checkpoint_and_rollback() {
        let home = temp_arreio_home();
        let store = ShadowGitStore::new(&home).unwrap();
        let worktree = home.join("project");
        init_git_worktree(&worktree);

        // Checkpoint inicial
        let hash1 = store.checkpoint(&worktree, "proj1").unwrap();
        assert!(!hash1.is_empty());

        // Modifica arquivo
        fs::write(worktree.join("file.txt"), "modified").unwrap();
        let hash2 = store.checkpoint(&worktree, "proj1").unwrap();
        assert_ne!(hash1, hash2);

        // Rollback para hash1
        store.rollback(&worktree, &hash1).unwrap();
        let content = fs::read_to_string(worktree.join("file.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn checkpoint_manager_maybe_checkpoint() {
        let home = temp_arreio_home();
        let worktree = home.join("project");
        init_git_worktree(&worktree);

        let mut mgr = CheckpointManager::new(&home).unwrap();
        let hash = mgr.maybe_checkpoint(&worktree, "proj1", true).unwrap();
        assert!(hash.is_some());
        assert_eq!(mgr.records().len(), 1);
    }

    #[test]
    fn checkpoint_manager_skips_non_mutating() {
        let home = temp_arreio_home();
        let mut mgr = CheckpointManager::new(&home).unwrap();
        let hash = mgr.maybe_checkpoint("/tmp", "proj1", false).unwrap();
        assert!(hash.is_none());
    }

    #[test]
    fn prune_orphaned_removes_missing_worktrees() {
        let home = temp_arreio_home();
        let store = ShadowGitStore::new(&home).unwrap();
        let worktree = home.join("existing");
        init_git_worktree(&worktree);

        store.checkpoint(&worktree, "existing").unwrap();

        // Prune sem worktrees conhecidos deve remover
        let pruned = store.prune_orphaned(&[]).unwrap();
        assert_eq!(pruned, 1);
    }

    #[test]
    fn checkpoint_serialization() {
        let record = CheckpointRecord {
            hash: "abc123".to_string(),
            project_hash: "proj1".to_string(),
            worktree: PathBuf::from("/tmp/proj"),
            created_at: 1234567890,
        };
        let json = serde_json::to_string(&record).unwrap();
        let restored: CheckpointRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record.hash, restored.hash);
    }
}
