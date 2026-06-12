//! Context References — sistema de @mentions para injeção de contexto.
//!
//! Inspirado no `agent/context_references.py` do Hermes Agent.
//! Suporta: @file:path[:line-range], @folder:path, @diff, @staged, @git:N, @url:...

use std::path::Path;

/// Parser de @mentions em texto do usuário.
pub struct ContextReferenceParser;

/// Referência de contexto parseada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextReference {
    /// @file:"path" ou @file:"path:10" ou @file:"path:10-20"
    File {
        path: String,
        line_start: Option<usize>,
        line_end: Option<usize>,
    },
    /// @folder:"path"
    Folder { path: String },
    /// @diff
    Diff,
    /// @staged
    Staged,
    /// @git:N
    Git { n: usize },
    /// @url:"..."
    Url { url: String },
}

impl ContextReferenceParser {
    /// Parse todas as @mentions em um texto.
    pub fn parse(text: &str) -> Vec<ContextReference> {
        let mut refs = Vec::new();
        // Regex para @file:"path" com range opcional
        let file_re = regex::Regex::new(r#"@file:"([^"]+)""#).unwrap();
        for cap in file_re.captures_iter(text) {
            let path_str = cap.get(1).unwrap().as_str();
            let (path, line_start, line_end) = Self::parse_file_ref(path_str);
            refs.push(ContextReference::File {
                path,
                line_start,
                line_end,
            });
        }
        // @folder:"path"
        let folder_re = regex::Regex::new(r#"@folder:"([^"]+)""#).unwrap();
        for cap in folder_re.captures_iter(text) {
            refs.push(ContextReference::Folder {
                path: cap.get(1).unwrap().as_str().to_string(),
            });
        }
        // @diff
        if text.contains("@diff") {
            refs.push(ContextReference::Diff);
        }
        // @staged
        if text.contains("@staged") {
            refs.push(ContextReference::Staged);
        }
        // @git:N
        let git_re = regex::Regex::new(r"@git:(\d+)").unwrap();
        for cap in git_re.captures_iter(text) {
            let n = cap.get(1).unwrap().as_str().parse().unwrap_or(1);
            refs.push(ContextReference::Git { n });
        }
        // @url:"..."
        let url_re = regex::Regex::new(r#"@url:"([^"]+)""#).unwrap();
        for cap in url_re.captures_iter(text) {
            refs.push(ContextReference::Url {
                url: cap.get(1).unwrap().as_str().to_string(),
            });
        }
        refs
    }

    fn parse_file_ref(s: &str) -> (String, Option<usize>, Option<usize>) {
        if let Some(colon) = s.rfind(':') {
            let path_part = &s[..colon];
            let range_part = &s[colon + 1..];
            if let Some(dash) = range_part.find('-') {
                let start = range_part[..dash].parse().ok();
                let end = range_part[dash + 1..].parse().ok();
                (path_part.to_string(), start, end)
            } else if let Ok(start) = range_part.parse::<usize>() {
                (path_part.to_string(), Some(start), Some(start))
            } else {
                (s.to_string(), None, None)
            }
        } else {
            (s.to_string(), None, None)
        }
    }
}

/// Resolvedor de referências — converte @mentions em texto injetável.
pub struct ContextReferenceResolver;

impl ContextReferenceResolver {
    /// Resolve uma referência em texto de contexto.
    pub fn resolve(
        reference: &ContextReference,
        allowed_root: &Path,
    ) -> Result<String, ReferenceError> {
        match reference {
            ContextReference::File {
                path,
                line_start,
                line_end,
            } => Self::resolve_file(path, *line_start, *line_end, allowed_root),
            ContextReference::Folder { path } => Self::resolve_folder(path, allowed_root),
            ContextReference::Diff => Ok("[git diff output would be injected here]".to_string()),
            ContextReference::Staged => {
                Ok("[git diff --staged output would be injected here]".to_string())
            }
            ContextReference::Git { n } => {
                Ok(format!("[git log -{} output would be injected here]", n))
            }
            ContextReference::Url { url } => {
                Ok(format!("[content from URL {} would be fetched here]", url))
            }
        }
    }

    fn resolve_file(
        path_str: &str,
        line_start: Option<usize>,
        line_end: Option<usize>,
        allowed_root: &Path,
    ) -> Result<String, ReferenceError> {
        let path = Path::new(path_str);
        // Path traversal check
        let canonical = path
            .canonicalize()
            .map_err(|_| ReferenceError::PathNotFound(path_str.to_string()))?;
        let root_canonical = allowed_root
            .canonicalize()
            .unwrap_or(allowed_root.to_path_buf());
        if !canonical.starts_with(&root_canonical) {
            return Err(ReferenceError::PathTraversal(path_str.to_string()));
        }
        // Sensitive file check
        let normalized = path_str.replace("\\", "/");
        let sensitive = [
            ".ssh",
            ".aws",
            ".env",
            ".kube",
            ".gnupg",
            "/etc/shadow",
            "/etc/passwd",
        ];
        if sensitive.iter().any(|s| normalized.contains(s)) {
            return Err(ReferenceError::SensitivePath(path_str.to_string()));
        }
        // Binary check
        if is_binary_file(path_str) {
            return Err(ReferenceError::BinaryFile(path_str.to_string()));
        }

        let content = std::fs::read_to_string(&canonical)
            .map_err(|e| ReferenceError::ReadError(path_str.to_string(), e.to_string()))?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let extracted = match (line_start, line_end) {
            (Some(start), Some(end)) => {
                let start_idx = start.saturating_sub(1);
                let end_idx = end.min(total_lines);
                lines[start_idx..end_idx].join("\n")
            }
            _ => content,
        };

        Ok(format!("```\n{}\n```", extracted))
    }

    fn resolve_folder(path_str: &str, allowed_root: &Path) -> Result<String, ReferenceError> {
        let path = Path::new(path_str);
        let canonical = path
            .canonicalize()
            .map_err(|_| ReferenceError::PathNotFound(path_str.to_string()))?;
        let root_canonical = allowed_root
            .canonicalize()
            .unwrap_or(allowed_root.to_path_buf());
        if !canonical.starts_with(&root_canonical) {
            return Err(ReferenceError::PathTraversal(path_str.to_string()));
        }

        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&canonical)
            .map_err(|e| ReferenceError::ReadError(path_str.to_string(), e.to_string()))?
        {
            if let Ok(entry) = entry {
                let name = entry.file_name().to_string_lossy().to_string();
                let file_type = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    "dir"
                } else {
                    "file"
                };
                entries.push(format!("{} ({})", name, file_type));
            }
        }
        entries.sort();
        entries.truncate(200); // max 200 entries

        Ok(format!(
            "Directory listing for {}:\n{}",
            path_str,
            entries.join("\n")
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError {
    PathNotFound(String),
    PathTraversal(String),
    SensitivePath(String),
    BinaryFile(String),
    ReadError(String, String),
}

impl std::fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReferenceError::PathNotFound(p) => write!(f, "Arquivo não encontrado: {}", p),
            ReferenceError::PathTraversal(p) => write!(f, "Path traversal bloqueado: {}", p),
            ReferenceError::SensitivePath(p) => write!(f, "Arquivo sensível bloqueado: {}", p),
            ReferenceError::BinaryFile(p) => write!(f, "Arquivo binário rejeitado: {}", p),
            ReferenceError::ReadError(p, e) => write!(f, "Erro lendo {}: {}", p, e),
        }
    }
}

impl std::error::Error for ReferenceError {}

fn is_binary_file(path: &str) -> bool {
    let binary_exts = [
        ".exe", ".dll", ".so", ".dylib", ".bin", ".o", ".obj", ".png", ".jpg", ".jpeg", ".gif",
        ".zip", ".tar", ".gz",
    ];
    let lower = path.to_lowercase();
    binary_exts.iter().any(|ext| lower.ends_with(ext))
}

/// Calcula o tamanho total do contexto injetado.
/// Soft limit: 25% da context window / Hard limit: 50%.
pub fn check_injection_budget(
    refs: &[ContextReference],
    context_window: usize,
) -> Result<(), String> {
    let soft_limit = context_window / 4;
    let hard_limit = context_window / 2;

    let mut total_chars = 0;
    for r in refs {
        total_chars += estimate_ref_size(r);
    }

    if total_chars > hard_limit {
        return Err(format!(
            "Contexto injetado excede hard limit ({} > {} chars)",
            total_chars, hard_limit
        ));
    }

    if total_chars > soft_limit {
        // Warning mas permite
        eprintln!(
            "[WARN] Contexto injetado excede soft limit ({} > {} chars)",
            total_chars, soft_limit
        );
    }

    Ok(())
}

fn estimate_ref_size(r: &ContextReference) -> usize {
    match r {
        ContextReference::File { .. } => 2_000,
        ContextReference::Folder { .. } => 500,
        ContextReference::Diff => 5_000,
        ContextReference::Staged => 3_000,
        ContextReference::Git { n } => n * 1_000,
        ContextReference::Url { .. } => 3_000,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_file_reference() {
        let text = r#"Check @file:"src/main.rs" for details"#;
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 1);
        assert!(
            matches!(refs[0], ContextReference::File { ref path, line_start: None, line_end: None } if path == "src/main.rs")
        );
    }

    #[test]
    fn parse_file_with_line() {
        let text = r#"See @file:"src/lib.rs:45" for the function"#;
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 1);
        assert!(matches!(
            refs[0],
            ContextReference::File {
                line_start: Some(45),
                line_end: Some(45),
                ..
            }
        ));
    }

    #[test]
    fn parse_file_with_range() {
        let text = r#"Check @file:"src/lib.rs:10-20" for the block"#;
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 1);
        assert!(matches!(
            refs[0],
            ContextReference::File {
                line_start: Some(10),
                line_end: Some(20),
                ..
            }
        ));
    }

    #[test]
    fn parse_folder() {
        let text = r#"List @folder:"src/" contents"#;
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 1);
        assert!(matches!(refs[0], ContextReference::Folder { ref path } if path == "src/"));
    }

    #[test]
    fn parse_diff() {
        let text = "Show @diff and @staged";
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 2);
        assert!(matches!(refs[0], ContextReference::Diff));
        assert!(matches!(refs[1], ContextReference::Staged));
    }

    #[test]
    fn parse_git_log() {
        let text = "Show @git:5 commits";
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 1);
        assert!(matches!(refs[0], ContextReference::Git { n: 5 }));
    }

    #[test]
    fn parse_url() {
        let text = r#"Fetch @url:"https://example.com/doc" for info"#;
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 1);
        assert!(
            matches!(refs[0], ContextReference::Url { ref url } if url == "https://example.com/doc")
        );
    }

    #[test]
    fn parse_multiple() {
        let text = r#"@file:"a.rs" and @file:"b.rs:10" and @diff"#;
        let refs = ContextReferenceParser::parse(text);
        assert_eq!(refs.len(), 3);
    }

    #[test]
    fn resolve_file_success() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.txt");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "line 1\nline 2\nline 3").unwrap();

        let reference = ContextReference::File {
            path: file_path.to_string_lossy().to_string(),
            line_start: Some(2),
            line_end: Some(3),
        };
        let result = ContextReferenceResolver::resolve(&reference, tmp.path());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("line 2"));
        assert!(!text.contains("line 1"));
    }

    #[test]
    fn resolve_path_traversal_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let reference = ContextReference::File {
            path: "/etc/passwd".to_string(),
            line_start: None,
            line_end: None,
        };
        let result = ContextReferenceResolver::resolve(&reference, tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn resolve_sensitive_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let reference = ContextReference::File {
            path: "~/.ssh/id_rsa".to_string(),
            line_start: None,
            line_end: None,
        };
        let result = ContextReferenceResolver::resolve(&reference, tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn resolve_binary_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let reference = ContextReference::File {
            path: "image.png".to_string(),
            line_start: None,
            line_end: None,
        };
        let result = ContextReferenceResolver::resolve(&reference, tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn budget_soft_limit_warns() {
        let refs = vec![
            ContextReference::Diff,
            ContextReference::Diff,
            ContextReference::Diff,
        ];
        // 3 * 5000 = 15000; soft limit de 1000 = 250; hard limit = 500
        let result = check_injection_budget(&refs, 1_000);
        assert!(result.is_err());
    }

    #[test]
    fn budget_within_limits() {
        let refs = vec![ContextReference::File {
            path: "a.rs".to_string(),
            line_start: None,
            line_end: None,
        }];
        let result = check_injection_budget(&refs, 10_000);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_folder_success() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::File::create(tmp.path().join("a.rs")).unwrap();
        std::fs::File::create(tmp.path().join("b.rs")).unwrap();

        let reference = ContextReference::Folder {
            path: tmp.path().to_string_lossy().to_string(),
        };
        let result = ContextReferenceResolver::resolve(&reference, tmp.path());
        assert!(result.is_ok());
        let text = result.unwrap();
        assert!(text.contains("a.rs"));
        assert!(text.contains("b.rs"));
    }
}
