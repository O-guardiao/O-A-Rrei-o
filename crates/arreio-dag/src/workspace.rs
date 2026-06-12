use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Gerencia git worktrees para isolamento de execução de nós do DAG.
/// Cada nó pode rodar em seu próprio worktree, evitando conflitos entre
/// tarefas paralelas (padrão Codex App).
pub struct WorkspaceManager {
    base_dir: PathBuf,
    worktrees_dir: PathBuf,
    allocations: HashMap<String, PathBuf>,
}

impl WorkspaceManager {
    /// Cria o manager com diretório base do projeto.
    pub fn new(work_dir: &Path) -> Result<Self> {
        let worktrees_dir = work_dir.join(".arreio").join("worktrees");
        std::fs::create_dir_all(&worktrees_dir)?;
        Ok(Self {
            base_dir: work_dir.to_path_buf(),
            worktrees_dir,
            allocations: HashMap::new(),
        })
    }

    /// Aloca um worktree para o nó. Se já existe, reutiliza.
    pub fn alloc(&mut self, node_id: &str) -> Result<PathBuf> {
        if let Some(path) = self.allocations.get(node_id) {
            return Ok(path.clone());
        }

        let wt_path = self.worktrees_dir.join(node_id);
        if wt_path.exists() {
            self.allocations
                .insert(node_id.to_string(), wt_path.clone());
            return Ok(wt_path);
        }

        // Verifica se estamos em um repo git
        if !self.base_dir.join(".git").exists() {
            bail!("diretório base não é um repositório git; worktrees requerem git");
        }

        let branch_name = format!("arreio/wt-{}", node_id);

        // Cria branch a partir do HEAD atual
        let branch = Command::new("git")
            .args(["branch", &branch_name])
            .current_dir(&self.base_dir)
            .output()
            .context("git branch falhou")?;
        if !branch.status.success() {
            // Branch pode já existir — ignora
            let stderr = String::from_utf8_lossy(&branch.stderr);
            if !stderr.contains("already exists") {
                bail!("git branch falhou: {}", stderr);
            }
        }

        // Cria worktree
        let wt = Command::new("git")
            .args(["worktree", "add", wt_path.to_str().unwrap(), &branch_name])
            .current_dir(&self.base_dir)
            .output()
            .context("git worktree add falhou")?;
        if !wt.status.success() {
            let stderr = String::from_utf8_lossy(&wt.stderr);
            if !stderr.contains("already exists") {
                bail!("git worktree add falhou: {}", stderr);
            }
        }

        self.allocations
            .insert(node_id.to_string(), wt_path.clone());
        Ok(wt_path)
    }

    /// Remove o worktree e a branch associada.
    pub fn release(&mut self, node_id: &str) -> Result<()> {
        let wt_path = self.worktrees_dir.join(node_id);
        if wt_path.exists() {
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force", wt_path.to_str().unwrap()])
                .current_dir(&self.base_dir)
                .output();
        }
        let branch_name = format!("arreio/wt-{}", node_id);
        let _ = Command::new("git")
            .args(["branch", "-D", &branch_name])
            .current_dir(&self.base_dir)
            .output();
        self.allocations.remove(node_id);
        Ok(())
    }

    /// Faz merge do worktree de volta para a branch principal.
    /// Retorna o path do worktree mergeado.
    pub fn merge_back(&self, node_id: &str) -> Result<PathBuf> {
        let branch_name = format!("arreio/wt-{}", node_id);
        let merge = Command::new("git")
            .args([
                "merge",
                "--no-ff",
                "-m",
                &format!("arreio: merge {}", node_id),
                &branch_name,
            ])
            .current_dir(&self.base_dir)
            .output()
            .context("git merge falhou")?;
        if !merge.status.success() {
            bail!(
                "git merge falhou: {}",
                String::from_utf8_lossy(&merge.stderr)
            );
        }
        Ok(self.worktrees_dir.join(node_id))
    }

    /// Retorna o path do worktree para o nó (se alocado).
    pub fn path_for(&self, node_id: &str) -> Option<PathBuf> {
        self.allocations.get(node_id).cloned()
    }

    /// Retorna o diretório base do projeto.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("git init");
        Command::new("git")
            .args([
                "-c",
                "user.email=test@test",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(dir)
            .output()
            .expect("git commit");
    }

    #[test]
    fn worktree_created_and_cleaned() {
        let dir = TempDir::new().unwrap();
        init_git_repo(dir.path());

        let mut mgr = WorkspaceManager::new(dir.path()).unwrap();
        let path = mgr.alloc("node-1").unwrap();
        assert!(path.exists());

        mgr.release("node-1").unwrap();
        // worktree pode deixar diretório vazio; branch deve ser removida
    }

    #[test]
    fn worktree_reusable() {
        let dir = TempDir::new().unwrap();
        init_git_repo(dir.path());

        let mut mgr = WorkspaceManager::new(dir.path()).unwrap();
        let p1 = mgr.alloc("node-a").unwrap();
        let p2 = mgr.alloc("node-a").unwrap();
        assert_eq!(p1, p2);
    }
}
