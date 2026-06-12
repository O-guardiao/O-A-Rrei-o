use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Gera diff real via `git diff` para inspeção.
/// Se o arquivo não existe no git (novo), gera diff unificado simulado.
pub fn generate_diff(file_path: &str, new_content: &str) -> Result<String> {
    let path = Path::new(file_path);

    // Verifica se arquivo existe no working tree do git
    let tracked = is_tracked_by_git(path)?;

    if tracked {
        // Salva conteúdo temporariamente, faz git diff, restaura
        let original = std::fs::read_to_string(path)
            .with_context(|| format!("lendo {} para diff", file_path))?;
        std::fs::write(path, new_content)
            .with_context(|| format!("escrevendo temp em {}", file_path))?;

        let output = Command::new("git")
            .args(["diff", "--no-color", "--", file_path])
            .output()
            .context("executando git diff")?;

        // Restaura original
        std::fs::write(path, original).with_context(|| format!("restaurando {}", file_path))?;

        if output.status.success() {
            let diff = String::from_utf8_lossy(&output.stdout);
            if diff.trim().is_empty() {
                Ok(format!(
                    "--- {}\n+++ {}\n@@ -1 +1 @@\n+{}",
                    file_path,
                    file_path,
                    new_content.lines().next().unwrap_or("")
                ))
            } else {
                Ok(diff.into_owned())
            }
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git diff falhou: {}", stderr)
        }
    } else {
        // Arquivo novo — diff simulado
        let mut diff = format!("--- /dev/null\n+++ {}\n", file_path);
        let lines: Vec<&str> = new_content.lines().collect();
        diff.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
        for line in lines {
            diff.push_str(&format!("+{}\n", line));
        }
        Ok(diff)
    }
}

fn is_tracked_by_git(path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["ls-files", "--error-unmatch", &path.to_string_lossy()])
        .output()
        .context("executando git ls-files")?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn diff_novo_arquivo() {
        let diff = generate_diff("src/main.rs", "fn main() {}\n").unwrap();
        assert!(diff.contains("+++ src/main.rs"));
        assert!(diff.contains("+fn main() {}"));
    }

    #[test]
    #[ignore = "requer repo git inicializado"]
    fn diff_arquivo_tracked() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("tracked.rs");
        fs::write(&file, "fn old() {}\n").unwrap();

        Command::new("git")
            .arg("init")
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&dir)
            .output()
            .unwrap();

        let diff = generate_diff(file.to_str().unwrap(), "fn new() {}\n").unwrap();
        assert!(diff.contains("-fn old() {}"));
        assert!(diff.contains("+fn new() {}"));
    }
}
