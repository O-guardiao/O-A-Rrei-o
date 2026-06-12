//! Engine principal de program slicing.

use anyhow::Result;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// Resultado de uma operação de slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceResult {
    /// Linhas relevantes (1-based), ordenadas.
    pub relevant_lines: Vec<usize>,
    /// Código-fonte original (para referência).
    pub source: String,
}

/// Direção do slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceDirection {
    /// Instruções que influenciam o critério.
    Backward,
    /// Instruções influenciadas pelo critério.
    Forward,
    /// União de Backward e Forward.
    Both,
}

/// Engine de slicing que orquestra backward/forward/both.
pub struct ProgramSlicer;

impl ProgramSlicer {
    /// Executa o slice conforme a direção especificada.
    pub fn slice(
        source: &str,
        criterion: &crate::SliceCriterion,
        direction: SliceDirection,
    ) -> Result<SliceResult> {
        criterion.validate(source)?;

        let relevant_lines = match direction {
            SliceDirection::Backward => {
                crate::backward_slice::backward_slice(source, criterion)?.relevant_lines
            }
            SliceDirection::Forward => {
                crate::forward_slice::forward_slice(source, criterion)?.relevant_lines
            }
            SliceDirection::Both => {
                let mut bwd =
                    crate::backward_slice::backward_slice(source, criterion)?.relevant_lines;
                let mut fwd =
                    crate::forward_slice::forward_slice(source, criterion)?.relevant_lines;
                bwd.append(&mut fwd);
                bwd.sort_unstable();
                bwd.dedup();
                bwd
            }
        };

        Ok(SliceResult {
            relevant_lines,
            source: source.to_string(),
        })
    }
}

/// Keywords Rust que não devem ser tratadas como variáveis.
static KEYWORDS: &[&str] = &[
    "let",
    "mut",
    "if",
    "else",
    "while",
    "for",
    "loop",
    "return",
    "fn",
    "struct",
    "enum",
    "impl",
    "match",
    "true",
    "false",
    "self",
    "Self",
    "i32",
    "u32",
    "i64",
    "u64",
    "f32",
    "f64",
    "usize",
    "isize",
    "bool",
    "String",
    "str",
    "char",
    "Vec",
    "Option",
    "Result",
    "pub",
    "use",
    "mod",
    "crate",
    "super",
    "as",
    "where",
    "type",
    "const",
    "static",
    "move",
    "ref",
    "break",
    "continue",
    "in",
    "async",
    "await",
    "yield",
    "macro",
    "union",
    "unsafe",
    "dyn",
    "println",
    "eprintln",
    "print",
    "format",
    "panic",
    "assert",
    "assert_eq",
    "vec",
    "Some",
    "None",
    "Ok",
    "Err",
];

/// Retorna o regex de atribuição compilado (lazy).
fn assign_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*(?:let\s+(?:mut\s+)?)?([a-zA-Z_][a-zA-Z0-9_]*)\s*=(.*)$").unwrap()
    })
}

/// Retorna o regex de identificadores compilado (lazy).
fn id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*").unwrap())
}

/// Analisa uma linha e retorna (variável atribuída, variáveis usadas).
pub fn parse_line(line: &str) -> (Option<String>, Vec<String>) {
    let line = strip_comments(line);

    let assign_re = assign_regex();
    let id_re = id_regex();

    let assigned = if let Some(cap) = assign_re.captures(&line) {
        let rhs = cap[2].trim_start();
        // Rejeita se o lado direito começar com '=' (caso de ==)
        if rhs.starts_with('=') {
            None
        } else {
            Some(cap[1].to_string())
        }
    } else {
        None
    };

    let rhs = if let Some(cap) = assign_re.captures(&line) {
        let r = cap[2].trim_start();
        if r.starts_with('=') {
            line.to_string()
        } else {
            cap[2].to_string()
        }
    } else {
        line.to_string()
    };

    let keywords: HashSet<&str> = KEYWORDS.iter().copied().collect();
    let mut used = Vec::new();
    for m in id_re.find_iter(&rhs) {
        let id = m.as_str();
        if !keywords.contains(id) && Some(id) != assigned.as_deref() {
            used.push(id.to_string());
        }
    }
    used.sort_unstable();
    used.dedup();

    (assigned, used)
}

/// Remove comentários de linha (`//`) da string.
fn strip_comments(line: &str) -> String {
    if let Some(pos) = line.find("//") {
        line[..pos].to_string()
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_line_let_binding() {
        let (assigned, used) = parse_line("let x = y + z;");
        assert_eq!(assigned, Some("x".to_string()));
        assert_eq!(used, vec!["y", "z"]);
    }

    #[test]
    fn test_parse_line_assignment() {
        let (assigned, used) = parse_line("a = b * 2;");
        assert_eq!(assigned, Some("a".to_string()));
        assert_eq!(used, vec!["b"]);
    }

    #[test]
    fn test_parse_line_no_assignment() {
        let (assigned, used) = parse_line("println!(\"{}\", x);");
        assert_eq!(assigned, None);
        assert_eq!(used, vec!["x"]);
    }

    #[test]
    fn test_parse_line_ignores_keywords() {
        let (assigned, used) = parse_line("if x > 0 { return true; }");
        assert_eq!(assigned, None);
        assert_eq!(used, vec!["x"]);
    }

    #[test]
    fn test_parse_line_ignores_comments() {
        let (assigned, used) = parse_line("let a = b; // comentário");
        assert_eq!(assigned, Some("a".to_string()));
        assert_eq!(used, vec!["b"]);
    }

    #[test]
    fn test_parse_line_ignores_equality() {
        let (assigned, used) = parse_line("if x == y {}");
        assert_eq!(assigned, None);
        assert_eq!(used, vec!["x", "y"]);
    }

    #[test]
    fn test_slice_direction_both_union() {
        let source = "let a = 1;\nlet b = a + 2;\nlet c = b + 3;";
        let criterion = crate::SliceCriterion::new(2, "b");
        let result = ProgramSlicer::slice(source, &criterion, SliceDirection::Both).unwrap();
        // backward: linha 2 (b) e 1 (a influencia b)
        // forward: linha 2 (b) e 3 (c usa b)
        assert_eq!(result.relevant_lines, vec![1, 2, 3]);
    }
}
