use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Gerencia arquivos de memória durável de projeto no estilo Codex.
/// Arquivos markdown em `.arreio/memory/` que o agente lê/escreve para manter
/// coerência entre sessões de longa duração (horas/dias).
///
/// Arquivos gerenciados:
/// - Prompt.md   → especificação congelada (Goals, Non-Goals, Constraints, Done-When)
/// - Plan.md     → milestones com acceptance criteria e validation commands
/// - Progress.md → log de progresso escrito pelo harness após cada nó
/// - Decision.log → decisões arquiteturais e notas de correção
pub struct ProjectMemory {
    base_dir: PathBuf,
}

impl ProjectMemory {
    /// Abre (ou cria) o sistema de memória de projeto no diretório `.arreio/memory/`.
    pub fn open(work_dir: &Path) -> Result<Self> {
        let base_dir = work_dir.join(".arreio").join("memory");
        fs::create_dir_all(&base_dir)?;
        Ok(Self { base_dir })
    }

    // ── Leitura ───────────────────────────────────────────────────────────────

    pub fn read_prompt(&self) -> Result<String> {
        self.read_file("Prompt.md")
    }

    pub fn read_plan(&self) -> Result<String> {
        self.read_file("Plan.md")
    }

    pub fn read_progress(&self) -> Result<String> {
        self.read_file("Progress.md")
    }

    pub fn read_decisions(&self) -> Result<String> {
        self.read_file("Decision.log")
    }

    fn read_file(&self, name: &str) -> Result<String> {
        let path = self.base_dir.join(name);
        if path.exists() {
            fs::read_to_string(&path).with_context(|| format!("lendo {}", path.display()))
        } else {
            Ok(String::new())
        }
    }

    // ── Escrita ───────────────────────────────────────────────────────────────

    pub fn write_prompt(&self, content: &str) -> Result<()> {
        self.write_file("Prompt.md", content)
    }

    pub fn write_plan(&self, content: &str) -> Result<()> {
        self.write_file("Plan.md", content)
    }

    /// Acrescenta uma linha de progresso ao Progress.md (append).
    pub fn append_progress(&self, entry: &str) -> Result<()> {
        let path = self.base_dir.join("Progress.md");
        let line = format!("{}\n", entry);
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())
            .with_context(|| format!("append Progress.md"))?;
        Ok(())
    }

    /// Acrescenta uma decisão ao Decision.log (append).
    pub fn append_decision(&self, entry: &str) -> Result<()> {
        let path = self.base_dir.join("Decision.log");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let line = format!("[{}] {}\n", timestamp, entry);
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())
            .with_context(|| format!("append Decision.log"))?;
        Ok(())
    }

    fn write_file(&self, name: &str, content: &str) -> Result<()> {
        let path = self.base_dir.join(name);
        fs::write(&path, content).with_context(|| format!("escrevendo {}", path.display()))
    }

    // ── Indexação para recall ─────────────────────────────────────────────────

    /// Retorna o conteúdo concatenado de todos os arquivos de memória,
    /// com cabeçalhos identificando a origem. Usado pelo RecallPipeline
    /// para indexar memória durável além do Blackboard.
    pub fn indexed_content(&self) -> Result<String> {
        let mut out = String::new();
        for (name, reader) in [
            (
                "Prompt.md",
                Self::read_prompt as fn(&Self) -> Result<String>,
            ),
            ("Plan.md", Self::read_plan as fn(&Self) -> Result<String>),
            (
                "Progress.md",
                Self::read_progress as fn(&Self) -> Result<String>,
            ),
            (
                "Decision.log",
                Self::read_decisions as fn(&Self) -> Result<String>,
            ),
        ] {
            match reader(self) {
                Ok(text) if !text.trim().is_empty() => {
                    out.push_str(&format!("\n--- {} ---\n{}", name, text));
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_pm() -> (ProjectMemory, TempDir) {
        let dir = TempDir::new().unwrap();
        let pm = ProjectMemory::open(dir.path()).unwrap();
        (pm, dir)
    }

    #[test]
    fn project_memory_roundtrip() {
        let (pm, _dir) = temp_pm();
        pm.write_prompt("# Goals\nBuild API").unwrap();
        assert_eq!(pm.read_prompt().unwrap().trim(), "# Goals\nBuild API");
    }

    #[test]
    fn append_progress_accumulates() {
        let (pm, _dir) = temp_pm();
        pm.append_progress("node-1: success").unwrap();
        pm.append_progress("node-2: failed").unwrap();
        let content = pm.read_progress().unwrap();
        assert!(content.contains("node-1: success"));
        assert!(content.contains("node-2: failed"));
    }

    #[test]
    fn indexed_content_includes_all() {
        let (pm, _dir) = temp_pm();
        pm.write_prompt("API spec").unwrap();
        pm.write_plan("milestone A").unwrap();
        pm.append_progress("done").unwrap();

        let idx = pm.indexed_content().unwrap();
        assert!(idx.contains("Prompt.md"));
        assert!(idx.contains("API spec"));
        assert!(idx.contains("Plan.md"));
        assert!(idx.contains("milestone A"));
        assert!(idx.contains("Progress.md"));
        assert!(idx.contains("done"));
    }

    #[test]
    fn missing_file_returns_empty() {
        let (pm, _dir) = temp_pm();
        assert!(pm.read_decisions().unwrap().is_empty());
    }
}
