use regex::Regex;
use std::collections::HashSet;

/// Nível de severidade de uma correspondência DLP.
#[derive(Debug, Clone, PartialEq)]
pub enum DlpSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Padrão de detecção de dados sensíveis.
#[derive(Debug, Clone)]
pub struct DlpPattern {
    pub name: String,
    pub regex: Regex,
    pub severity: DlpSeverity,
}

/// Correspondência encontrada pelo motor DLP.
#[derive(Debug, Clone)]
pub struct DlpMatch {
    pub pattern_name: String,
    pub severity: DlpSeverity,
    pub matched_text: String,
    pub position: (usize, usize),
}

/// Engine de DLP que detecta dados sensíveis em textos.
pub struct DlpEngine {
    patterns: Vec<DlpPattern>,
}

impl DlpEngine {
    /// Cria um novo engine vazio (sem padrões).
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Cria um engine pré-configurado com os padrões padrão.
    pub fn with_defaults() -> Self {
        Self {
            patterns: Self::default_patterns(),
        }
    }

    /// Adiciona um padrão customizado ao engine.
    pub fn add_pattern(&mut self, pattern: DlpPattern) {
        self.patterns.push(pattern);
    }

    /// Retorna os padrões padrão de detecção.
    pub fn default_patterns() -> Vec<DlpPattern> {
        vec![
            DlpPattern {
                name: "CPF".to_string(),
                regex: Regex::new(r"\d{3}\.\d{3}\.\d{3}-\d{2}|\d{11}").unwrap(),
                severity: DlpSeverity::High,
            },
            DlpPattern {
                name: "Email".to_string(),
                regex: Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap(),
                severity: DlpSeverity::Medium,
            },
            DlpPattern {
                name: "SSN".to_string(),
                regex: Regex::new(r"\d{3}-\d{2}-\d{4}").unwrap(),
                severity: DlpSeverity::High,
            },
            DlpPattern {
                name: "APIKey".to_string(),
                regex: Regex::new(
                    r#"(?i)(api[_-]?key|token|secret)\s*[:=]\s*['"]?[a-zA-Z0-9_-]{16,}['"]?"#,
                )
                .unwrap(),
                severity: DlpSeverity::Critical,
            },
            DlpPattern {
                name: "CreditCard".to_string(),
                regex: Regex::new(r"\d{4}[\s-]?\d{4}[\s-]?\d{4}[\s-]?\d{4}").unwrap(),
                severity: DlpSeverity::Critical,
            },
            DlpPattern {
                name: "AWSAccessKey".to_string(),
                regex: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
                severity: DlpSeverity::Critical,
            },
        ]
    }

    /// Escaneia o texto e retorna todas as correspondências encontradas.
    pub fn scan(&self, text: &str) -> Vec<DlpMatch> {
        let mut matches = Vec::new();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();

        for pattern in &self.patterns {
            for mat in pattern.regex.find_iter(text) {
                let pos = (mat.start(), mat.end());
                // Evita duplicatas exatas de posição
                if seen.insert(pos) {
                    matches.push(DlpMatch {
                        pattern_name: pattern.name.clone(),
                        severity: pattern.severity.clone(),
                        matched_text: mat.as_str().to_string(),
                        position: pos,
                    });
                }
            }
        }

        // Ordena por posição para facilitar leitura
        matches.sort_by_key(|m| m.position.0);
        matches
    }

    /// Retorna `true` se algum dado sensível for detectado.
    pub fn has_sensitive_data(&self, text: &str) -> bool {
        self.patterns.iter().any(|p| p.regex.is_match(text))
    }
}

impl Default for DlpEngine {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> DlpEngine {
        DlpEngine::with_defaults()
    }

    #[test]
    fn scan_cpf_com_pontos() {
        let e = engine();
        let text = "Meu CPF é 529.982.247-25.";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "CPF");
        assert_eq!(matches[0].matched_text, "529.982.247-25");
        assert_eq!(matches[0].severity, DlpSeverity::High);
    }

    #[test]
    fn scan_cpf_sem_pontos() {
        let e = engine();
        let text = "CPF: 52998224725";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].matched_text, "52998224725");
    }

    #[test]
    fn scan_email() {
        let e = engine();
        let text = "Contato: joao.silva@example.com.br";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "Email");
        assert_eq!(matches[0].severity, DlpSeverity::Medium);
    }

    #[test]
    fn scan_ssn() {
        let e = engine();
        let text = "SSN: 123-45-6789";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "SSN");
        assert_eq!(matches[0].matched_text, "123-45-6789");
    }

    #[test]
    fn scan_api_key() {
        let e = engine();
        let text = "api_key = 'abcdef1234567890'";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "APIKey");
        assert!(matches[0].matched_text.contains("abcdef1234567890"));
    }

    #[test]
    fn scan_credit_card() {
        let e = engine();
        let text = "Cartão: 4111 1111 1111 1111";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "CreditCard");
        assert_eq!(matches[0].severity, DlpSeverity::Critical);
    }

    #[test]
    fn scan_aws_access_key() {
        let e = engine();
        let text = "AWS Access Key: AKIAIOSFODNN7EXAMPLE";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "AWSAccessKey");
        assert_eq!(matches[0].matched_text, "AKIAIOSFODNN7EXAMPLE");
    }

    #[test]
    fn scan_multiplos_dados() {
        let e = engine();
        let text = "Email: ana@example.com e CPF 111.222.333-44";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].pattern_name, "Email");
        assert_eq!(matches[1].pattern_name, "CPF");
    }

    #[test]
    fn has_sensitive_data_true() {
        let e = engine();
        assert!(e.has_sensitive_data("token: secret_1234567890123456"));
    }

    #[test]
    fn has_sensitive_data_false() {
        let e = engine();
        assert!(!e.has_sensitive_data("Texto completamente inofensivo."));
    }

    #[test]
    fn scan_posicao_correta() {
        let e = engine();
        let text = "abc 123-45-6789 def";
        let matches = e.scan(text);
        assert_eq!(matches[0].position, (4, 15));
    }

    #[test]
    fn engine_vazio_nao_detecta() {
        let e = DlpEngine::new();
        assert!(!e.has_sensitive_data("529.982.247-25"));
        assert!(e.scan("qualquer coisa").is_empty());
    }

    #[test]
    fn adiciona_pattern_custom() {
        let mut e = DlpEngine::new();
        e.add_pattern(DlpPattern {
            name: "Telefone".to_string(),
            regex: Regex::new(r"\(\d{2}\)\s*\d{4,5}-\d{4}").unwrap(),
            severity: DlpSeverity::Low,
        });
        let text = "Ligue para (11) 91234-5678";
        let matches = e.scan(text);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].pattern_name, "Telefone");
    }
}
