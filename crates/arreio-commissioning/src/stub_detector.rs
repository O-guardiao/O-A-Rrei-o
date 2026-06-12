//! StubDetector — detecção estática de stubs e incompletudes (PVC-Q3.3).
//!
//! Papel do "Inspector" no Self-Commissioning: varre o código-fonte em busca
//! de marcadores de trabalho incompleto, aplicando a regra PVC
//! "incompleto oculto não pode". Determinístico: sem LLM, sem rede;
//! resultados ordenados por arquivo e linha.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Tipo de marcador encontrado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StubKind {
    /// `todo!(` — código que entra em pânico se executado.
    TodoMacro,
    /// `unimplemented!(` — idem.
    UnimplementedMacro,
    /// Comentário `TODO`.
    TodoComment,
    /// Comentário `FIXME`.
    FixmeComment,
}

impl StubKind {
    /// Severidade: macros panicam em runtime (alta); comentários são dívida (baixa).
    pub fn is_high_severity(&self) -> bool {
        matches!(self, StubKind::TodoMacro | StubKind::UnimplementedMacro)
    }
}

/// Ocorrência de stub no código.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StubFinding {
    /// Caminho relativo à raiz varrida.
    pub file: String,
    /// Linha 1-based.
    pub line: usize,
    pub kind: StubKind,
    /// Trecho da linha (truncado a 160 chars).
    pub snippet: String,
}

/// Relatório agregado da varredura.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StubReport {
    pub files_scanned: usize,
    pub findings: Vec<StubFinding>,
    pub high_severity_count: usize,
    pub low_severity_count: usize,
}

/// Detector estático de stubs.
pub struct StubDetector {
    /// Diretórios (por nome) ignorados na varredura.
    excluded_dirs: Vec<String>,
}

impl StubDetector {
    pub fn new() -> Self {
        Self {
            excluded_dirs: vec![
                "target".into(),
                "vendor".into(),
                ".git".into(),
                "node_modules".into(),
                ".arreio".into(),
            ],
        }
    }

    pub fn with_excluded_dirs(mut self, dirs: Vec<String>) -> Self {
        self.excluded_dirs = dirs;
        self
    }

    /// Varre recursivamente `root` procurando marcadores em arquivos `.rs`.
    pub fn scan(&self, root: &Path) -> Result<StubReport> {
        let mut files = Vec::new();
        self.collect_rs_files(root, &mut files)?;
        files.sort(); // ordem determinística

        let mut findings = Vec::new();
        for file in &files {
            let content = fs::read_to_string(file)
                .with_context(|| format!("lendo {}", file.display()))?;
            let relative = file
                .strip_prefix(root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            Self::scan_content(&content, &relative, &mut findings);
        }

        let high = findings.iter().filter(|f| f.kind.is_high_severity()).count();
        let low = findings.len() - high;
        Ok(StubReport {
            files_scanned: files.len(),
            findings,
            high_severity_count: high,
            low_severity_count: low,
        })
    }

    fn collect_rs_files(&self, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let entries = fs::read_dir(dir).with_context(|| format!("listando {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if self.excluded_dirs.iter().any(|d| d == &name) {
                    continue;
                }
                self.collect_rs_files(&path, out)?;
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                out.push(path);
            }
        }
        Ok(())
    }

    /// Varre o conteúdo de um arquivo. Público para testes com fontes sintéticas.
    pub fn scan_content(content: &str, file_label: &str, findings: &mut Vec<StubFinding>) {
        for (i, line) in content.lines().enumerate() {
            let line_no = i + 1;
            let trimmed = line.trim();

            // Macros de pânico: presença literal no código (não em comentário).
            let code_part = match trimmed.find("//") {
                Some(pos) => &trimmed[..pos],
                None => trimmed,
            };
            if code_part.contains("todo!(") {
                findings.push(Self::finding(file_label, line_no, StubKind::TodoMacro, trimmed));
            }
            if code_part.contains("unimplemented!(") {
                findings.push(Self::finding(
                    file_label,
                    line_no,
                    StubKind::UnimplementedMacro,
                    trimmed,
                ));
            }

            // Comentários de dívida: apenas dentro do comentário.
            if let Some(pos) = trimmed.find("//") {
                let comment = &trimmed[pos..];
                if comment.contains("TODO") {
                    findings.push(Self::finding(
                        file_label,
                        line_no,
                        StubKind::TodoComment,
                        trimmed,
                    ));
                }
                if comment.contains("FIXME") {
                    findings.push(Self::finding(
                        file_label,
                        line_no,
                        StubKind::FixmeComment,
                        trimmed,
                    ));
                }
            }
        }
    }

    fn finding(file: &str, line: usize, kind: StubKind, snippet: &str) -> StubFinding {
        StubFinding {
            file: file.to_string(),
            line,
            kind,
            snippet: snippet.chars().take(160).collect(),
        }
    }
}

impl Default for StubDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_todo_macro() {
        let src = "fn f() {\n    todo!(\"implementar\")\n}\n";
        let mut findings = Vec::new();
        StubDetector::scan_content(src, "f.rs", &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, StubKind::TodoMacro);
        assert_eq!(findings[0].line, 2);
        assert!(findings[0].kind.is_high_severity());
    }

    #[test]
    fn detecta_unimplemented_macro() {
        let src = "fn g() { unimplemented!() }";
        let mut findings = Vec::new();
        StubDetector::scan_content(src, "g.rs", &mut findings);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, StubKind::UnimplementedMacro);
    }

    #[test]
    fn detecta_comentarios_todo_fixme() {
        let src = "let x = 1; // TODO: revisar\n// FIXME corrigir antes do release\n";
        let mut findings = Vec::new();
        StubDetector::scan_content(src, "c.rs", &mut findings);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].kind, StubKind::TodoComment);
        assert_eq!(findings[1].kind, StubKind::FixmeComment);
        assert!(!findings[0].kind.is_high_severity());
    }

    #[test]
    fn todo_macro_em_comentario_nao_conta_como_macro() {
        let src = "// exemplo: todo!(\"x\") não deve contar como macro\n";
        let mut findings = Vec::new();
        StubDetector::scan_content(src, "c.rs", &mut findings);
        // Conta apenas como TodoComment ("TODO" não aparece em maiúsculas? "todo!(" minúsculo)
        // A linha não contém "TODO" maiúsculo → nenhum finding.
        assert!(findings.iter().all(|f| f.kind != StubKind::TodoMacro));
    }

    #[test]
    fn codigo_limpo_sem_findings() {
        let src = "fn soma(a: i32, b: i32) -> i32 { a + b }\n";
        let mut findings = Vec::new();
        StubDetector::scan_content(src, "ok.rs", &mut findings);
        assert!(findings.is_empty());
    }

    #[test]
    fn scan_de_diretorio_real_com_exclusao() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn f() { todo!() }\n").unwrap();
        // target/ deve ser ignorado mesmo contendo stubs
        std::fs::write(root.join("target/gen.rs"), "fn g() { todo!() }\n").unwrap();
        std::fs::write(root.join("README.md"), "todo!() em markdown não conta").unwrap();

        let report = StubDetector::new().scan(root).unwrap();
        assert_eq!(report.files_scanned, 1);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].file, "src/lib.rs");
        assert_eq!(report.high_severity_count, 1);
        assert_eq!(report.low_severity_count, 0);
    }
}
