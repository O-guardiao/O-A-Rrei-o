use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Antes de alterar qualquer arquivo: salva checkpoint git.
/// Em falha: reverte para o checkpoint anterior.
/// Inspirado no JCL + processamento em lote do spec.
pub struct Checkpoint;

impl Checkpoint {
    /// Cria um commit no repositório git do `work_dir`.
    pub fn save(node_id: &str, work_dir: &Path) -> Result<()> {
        // Garante que há um repo git
        if !work_dir.join(".git").exists() {
            let init = Command::new("git")
                .args(["init"])
                .current_dir(work_dir)
                .output()
                .context("git init falhou")?;
            if !init.status.success() {
                bail!("git init falhou: {}", String::from_utf8_lossy(&init.stderr));
            }
        }

        // Stage tudo
        let add = Command::new("git")
            .args(["add", "-A"])
            .current_dir(work_dir)
            .output()
            .context("git add -A falhou")?;
        if !add.status.success() {
            bail!("git add falhou: {}", String::from_utf8_lossy(&add.stderr));
        }

        // Commit (ignorar "nothing to commit")
        let msg = format!("arreio:ckpt:{}", node_id);
        let commit = Command::new("git")
            .args([
                "-c",
                "user.email=arreio@harness",
                "-c",
                "user.name=Arreio",
                "commit",
                "--allow-empty",
                "-m",
                &msg,
            ])
            .current_dir(work_dir)
            .output()
            .context("git commit falhou")?;

        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            // "nothing to commit" não é erro real
            if !stderr.contains("nothing to commit") {
                bail!("git commit falhou: {}", stderr);
            }
        }
        Ok(())
    }

    /// Reverte para o commit anterior (HEAD~1).
    pub fn rollback(work_dir: &Path) -> Result<()> {
        let reset = Command::new("git")
            .args(["reset", "--hard", "HEAD~1"])
            .current_dir(work_dir)
            .output()
            .context("git reset falhou")?;

        if !reset.status.success() {
            bail!(
                "git reset --hard falhou: {}",
                String::from_utf8_lossy(&reset.stderr)
            );
        }
        Ok(())
    }

    /// Retorna o hash do commit HEAD atual.
    pub fn current_hash(work_dir: &Path) -> Result<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(work_dir)
            .output()
            .context("git rev-parse falhou")?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
