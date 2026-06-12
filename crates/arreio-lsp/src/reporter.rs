use crate::baseline_store::Diagnostic;

/// Formata diagnósticos no formato compacto para atores.
pub struct DiagnosticReporter;

impl DiagnosticReporter {
    pub fn format(
        diagnostics: &[Diagnostic],
        file: &str,
        max_per_file: usize,
        max_total_chars: usize,
    ) -> String {
        let mut lines = vec![format!("<diagnostics file=\"{}\">", file)];
        let mut total_chars = lines[0].len();

        for diag in diagnostics.iter().take(max_per_file) {
            let severity = match diag.severity.as_str() {
                "ERROR" => "ERROR",
                "WARNING" => "WARN",
                _ => "INFO",
            };
            let code_str = diag.code.as_deref().unwrap_or("?");
            let source_str = diag.source.as_deref().unwrap_or("?");
            let line = format!(
                "{} [{}:{}] {} [{}] ({})",
                severity,
                diag.line,
                diag.column,
                truncate_msg(&diag.message, 120),
                code_str,
                source_str
            );
            if total_chars + line.len() + 1 > max_total_chars {
                lines.push(format!(
                    "... ({} more diagnostics)",
                    diagnostics.len() - lines.len() + 1
                ));
                break;
            }
            total_chars += line.len() + 1;
            lines.push(line);
        }

        lines.push("</diagnostics>".to_string());
        lines.join("\n")
    }

    /// Filtra apenas severidade ERROR por default.
    pub fn filter_errors(diagnostics: &[Diagnostic]) -> Vec<Diagnostic> {
        diagnostics
            .iter()
            .filter(|d| d.severity == "ERROR")
            .cloned()
            .collect()
    }
}

fn truncate_msg(msg: &str, max_len: usize) -> String {
    if msg.len() <= max_len {
        msg.to_string()
    } else {
        format!("{}...", &msg[..max_len])
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diag(line: usize, severity: &str, msg: &str) -> Diagnostic {
        Diagnostic {
            file: "test.rs".to_string(),
            line,
            column: 5,
            severity: severity.to_string(),
            message: msg.to_string(),
            code: Some("E0001".to_string()),
            source: Some("rustc".to_string()),
        }
    }

    #[test]
    fn format_diagnostics() {
        let diags = vec![
            make_diag(12, "ERROR", "cannot borrow"),
            make_diag(15, "WARNING", "unused variable"),
        ];
        let report = DiagnosticReporter::format(&diags, "src/main.rs", 20, 4000);
        assert!(report.contains("<diagnostics file=\"src/main.rs\">"));
        assert!(report.contains("ERROR [12:5]"));
        assert!(report.contains("WARN [15:5]"));
        assert!(report.contains("</diagnostics>"));
    }

    #[test]
    fn filter_errors_only() {
        let diags = vec![
            make_diag(1, "ERROR", "e1"),
            make_diag(2, "WARNING", "w1"),
            make_diag(3, "ERROR", "e2"),
        ];
        let errors = DiagnosticReporter::filter_errors(&diags);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn truncation_limits_output() {
        let diags = vec![make_diag(1, "ERROR", &"x".repeat(200))];
        let report = DiagnosticReporter::format(&diags, "test.rs", 20, 4000);
        assert!(report.contains("..."));
        assert!(!report.contains(&"x".repeat(200)));
    }
}
