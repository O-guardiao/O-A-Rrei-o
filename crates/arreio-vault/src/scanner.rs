use regex::Regex;
use std::collections::HashSet;

/// Padrões de secrets detectáveis em código/texto gerado.
pub struct SecretScanner {
    patterns: Vec<(String, Regex)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    pub pattern_name: String,
    pub matched_text: String,
    pub line_number: usize,
    pub severity: SecretSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSeverity {
    Critical, // AWS key, private key
    High,     // API token, database URL com senha
    Medium,   // password field, secret keyword
}

impl SecretScanner {
    pub fn new() -> Self {
        let patterns = vec![
            (
                "AWS Access Key".into(),
                Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
            ),
            (
                "AWS Secret Key".into(),
                Regex::new(r#"[\s"'][0-9a-zA-Z/+]{40}[\s"']"#).unwrap(),
            ),
            (
                "Private Key".into(),
                Regex::new(r"-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----").unwrap(),
            ),
            (
                "GitHub Token".into(),
                Regex::new(r"gh[pousr]_[A-Za-z0-9_]{36,}").unwrap(),
            ),
            (
                "Generic API Key".into(),
                Regex::new(r#"(?i)(api[_-]?key|apikey)\s*[:=]\s*['"][a-z0-9]{16,}['"]"#).unwrap(),
            ),
            (
                "Password Assignment".into(),
                Regex::new(r#"(?i)(password|passwd|pwd)\s*[:=]\s*['"][^'"]{4,}['"]"#).unwrap(),
            ),
            (
                "Database URL".into(),
                Regex::new(r#"(?i)(postgres|mysql|mongodb)://[^:]+:[^@]+@"#).unwrap(),
            ),
        ];
        Self { patterns }
    }

    /// Escaneia texto e retorna findings únicos (por matched_text).
    pub fn scan(&self, text: &str) -> Vec<SecretFinding> {
        let mut seen = HashSet::new();
        let mut findings = Vec::new();

        for (line_num, line) in text.lines().enumerate() {
            for (name, re) in &self.patterns {
                for mat in re.find_iter(line) {
                    let matched = mat.as_str().to_string();
                    if seen.insert(matched.clone()) {
                        let severity = classify_severity(name);
                        findings.push(SecretFinding {
                            pattern_name: name.clone(),
                            matched_text: matched,
                            line_number: line_num + 1,
                            severity,
                        });
                    }
                }
            }
        }

        findings.sort_by(|a, b| {
            let ord = severity_order(b.severity).cmp(&severity_order(a.severity));
            if ord == std::cmp::Ordering::Equal {
                a.line_number.cmp(&b.line_number)
            } else {
                ord
            }
        });
        findings
    }
}

fn classify_severity(name: &str) -> SecretSeverity {
    match name {
        "AWS Access Key" | "AWS Secret Key" | "Private Key" => SecretSeverity::Critical,
        "GitHub Token" | "Database URL" => SecretSeverity::High,
        _ => SecretSeverity::Medium,
    }
}

fn severity_order(s: SecretSeverity) -> u8 {
    match s {
        SecretSeverity::Critical => 3,
        SecretSeverity::High => 2,
        SecretSeverity::Medium => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_aws_key() {
        let scanner = SecretScanner::new();
        let text = "aws_access_key_id = AKIAIOSFODNN7EXAMPLE\n";
        let finds = scanner.scan(text);
        assert!(!finds.is_empty());
        assert_eq!(finds[0].pattern_name, "AWS Access Key");
        assert_eq!(finds[0].severity, SecretSeverity::Critical);
    }

    #[test]
    fn detecta_password() {
        let scanner = SecretScanner::new();
        let text = r#"password = "supersecret123"
"#;
        let finds = scanner.scan(text);
        assert!(!finds.is_empty());
        assert_eq!(finds[0].pattern_name, "Password Assignment");
    }

    #[test]
    fn nao_detecta_texto_inocente() {
        let scanner = SecretScanner::new();
        let text = "fn main() { println!(\"hello\"); }\n";
        let finds = scanner.scan(text);
        assert!(finds.is_empty());
    }
}
