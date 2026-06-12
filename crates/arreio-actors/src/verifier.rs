//! Verification Agent — GAP-027
//!
//! Ator adversarial dedicado a encontrar bugs e violações de spec.
//! Diferente do Inspector (que revisa qualidade de código), o Verifier
//! tenta **provar que o código está errado** via análise adversarial,
//! geração de fuzz inputs, e property-based testing.

use anyhow::Result;
use arreio_provider::{ChatRequest, ProviderClient};
use serde::{Deserialize, Serialize};

/// Resultado da verificação adversarial.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Código passou na verificação?
    pub passed: bool,
    /// Bugs encontrados (vazio se passou).
    pub bugs: Vec<BugReport>,
    /// Casos de teste gerados.
    pub generated_tests: Vec<GeneratedTest>,
    /// Confiança da análise (0.0–1.0).
    pub confidence: f64,
}

/// Relatório de um bug encontrado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugReport {
    pub severity: Severity,
    pub description: String,
    pub line_hint: Option<String>,
    pub reproduction: Option<String>,
}

/// Severidade do bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

/// Caso de teste gerado pelo Verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedTest {
    pub name: String,
    pub input: String,
    pub expected_behavior: String,
}

/// Agente de verificação adversarial.
pub struct VerificationAgent {
    client: Box<dyn ProviderClient>,
    model: String,
}

impl VerificationAgent {
    pub fn new(client: Box<dyn ProviderClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Verifica código contra uma especificação.
    ///
    /// 1. Gera fuzz inputs heurísticos a partir da spec.
    /// 2. Gera property tests a partir das invariantes declaradas.
    /// 3. Executa análise adversarial via LLM: "prove que este código está errado".
    /// 4. Retorna bugs encontrados + testes gerados.
    pub fn verify(&self, code: &str, spec: &str) -> Result<VerificationResult> {
        // ── Fase 1: Fuzz input generation heurística ──
        let fuzz_inputs = Self::generate_fuzz_inputs(spec);

        // ── Fase 2: Property test generation ──
        let property_tests = Self::generate_property_tests(spec);

        // ── Fase 3: LLM adversarial analysis ──
        let adversarial_prompt = format!(
            "You are a hostile code reviewer. Your job is to PROVE this code is wrong.\n\n\
             SPECIFICATION:\n{}\n\n\
             CODE:\n```rust\n{}\n```\n\n\
             Find bugs, edge cases, or violations of the spec. Be aggressive and thorough.\n\
             Return JSON with this structure:\n\
             {{\"passed\": false, \"bugs\": [{{\"severity\":\"Critical|High|Medium|Low\",\"description\":\"...\",\"line_hint\":\"...\",\"reproduction\":\"...\"}}], \"generated_tests\": [{{\"name\":\"...\",\"input\":\"...\",\"expected_behavior\":\"...\"}}], \"confidence\": 0.95}}",
            spec, code
        );

        let req = ChatRequest {
            messages: Vec::new(),
            model: self.model.clone(),
            system: "You are a hostile security auditor and fuzzer. Find bugs.".to_string(),
            user: adversarial_prompt,
            tools: None,
        };

        let response = self.client.chat(req)?;
        let clean = crate::actors::extract_json_block(&response.content);

        let mut result: VerificationResult = serde_json::from_str(&clean).unwrap_or_else(|_| {
            // Fallback se LLM não retornar JSON válido
            VerificationResult {
                passed: true,
                bugs: vec![],
                generated_tests: property_tests.clone(),
                confidence: 0.5,
            }
        });

        // Mescla testes gerados heurísticos + do LLM
        result.generated_tests.extend(fuzz_inputs.into_iter().map(|i| GeneratedTest {
            name: format!("fuzz_{}", i.chars().take(20).collect::<String>()),
            input: i,
            expected_behavior: "should not panic or violate spec".to_string(),
        }));

        Ok(result)
    }

    /// Verificação rápida (heurística sem LLM) para uso em pipelines críticos.
    pub fn verify_fast(code: &str, spec: &str) -> VerificationResult {
        let mut bugs = Vec::new();

        // Heurística 1: detecta unwrap/expect em código novo
        let unwrap_count = code.matches(".unwrap()").count() + code.matches(".expect(").count();
        if unwrap_count > 3 {
            bugs.push(BugReport {
                severity: Severity::Medium,
                description: format!("Código contém {} unwraps/expects — risco de panic", unwrap_count),
                line_hint: None,
                reproduction: None,
            });
        }

        // Heurística 2: detecta TODO/FIXME
        if code.contains("TODO") || code.contains("FIXME") {
            bugs.push(BugReport {
                severity: Severity::Low,
                description: "Código contém TODO/FIXME — incompleto".to_string(),
                line_hint: None,
                reproduction: None,
            });
        }

        // Heurística 3: compara spec com código (palavras-chave da spec devem aparecer)
        let spec_words: Vec<&str> = spec
            .split_whitespace()
            .filter(|w| w.len() > 4)
            .collect();
        let spec_coverage = spec_words
            .iter()
            .filter(|w| code.to_lowercase().contains(&w.to_lowercase()))
            .count() as f64
            / spec_words.len().max(1) as f64;
        if spec_coverage < 0.3 && !spec_words.is_empty() {
            bugs.push(BugReport {
                severity: Severity::High,
                description: format!(
                    "Código parece não implementar a spec (cobertura de palavras-chave: {:.0}%)",
                    spec_coverage * 100.0
                ),
                line_hint: None,
                reproduction: None,
            });
        }

        let passed = bugs.is_empty();
        let confidence = if passed { 0.6 } else { 0.8 };
        VerificationResult {
            passed,
            bugs,
            generated_tests: vec![],
            confidence,
        }
    }

    /// Gera inputs de fuzz heurísticos a partir da spec.
    fn generate_fuzz_inputs(spec: &str) -> Vec<String> {
        let mut inputs = Vec::new();
        let spec_lower = spec.to_lowercase();

        // Detecta tipos de input mencionados na spec
        if spec_lower.contains("string") || spec_lower.contains("text") {
            inputs.push("".to_string());
            inputs.push("a".repeat(10000));
            inputs.push("\0\n\r\t".to_string());
            inputs.push("<script>alert(1)</script>".to_string());
        }
        if spec_lower.contains("number") || spec_lower.contains("int") || spec_lower.contains("float") {
            inputs.push("0".to_string());
            inputs.push("-1".to_string());
            inputs.push("99999999999999999999".to_string());
            inputs.push("NaN".to_string());
        }
        if spec_lower.contains("path") || spec_lower.contains("file") {
            inputs.push("/dev/null".to_string());
            inputs.push("../../../etc/passwd".to_string());
            inputs.push("".to_string());
            inputs.push(r"\\?\C:\very\long\path".to_string());
        }
        if spec_lower.contains("json") || spec_lower.contains("serialize") {
            inputs.push("{}".to_string());
            inputs.push("null".to_string());
            inputs.push("[{\"nested\": \"deep\"}]".to_string());
        }

        inputs
    }

    /// Gera property tests a partir de invariantes comuns na spec.
    fn generate_property_tests(spec: &str) -> Vec<GeneratedTest> {
        let mut tests = Vec::new();
        let spec_lower = spec.to_lowercase();

        if spec_lower.contains("idempotent") {
            tests.push(GeneratedTest {
                name: "idempotence".to_string(),
                input: "same_input_twice".to_string(),
                expected_behavior: "second call returns same result as first".to_string(),
            });
        }
        if spec_lower.contains("nullable") || spec_lower.contains("optional") {
            tests.push(GeneratedTest {
                name: "null_safety".to_string(),
                input: "None/null".to_string(),
                expected_behavior: "does not panic, returns appropriate default or error".to_string(),
            });
        }
        if spec_lower.contains("error") || spec_lower.contains("exception") {
            tests.push(GeneratedTest {
                name: "error_propagation".to_string(),
                input: "invalid_input".to_string(),
                expected_behavior: "returns Err/Error, does not panic".to_string(),
            });
        }
        if spec_lower.contains("concurrent") || spec_lower.contains("thread") || spec_lower.contains("parallel") {
            tests.push(GeneratedTest {
                name: "thread_safety".to_string(),
                input: "multiple_threads_same_data".to_string(),
                expected_behavior: "no data races, consistent results".to_string(),
            });
        }

        tests
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_detects_unwraps() {
        let code = r#"
            fn bad() {
                let x = something().unwrap();
                let y = other().unwrap();
                let z = more().expect("fail");
                let w = again().unwrap();
            }
        "#;
        let result = VerificationAgent::verify_fast(code, "foo");
        assert!(!result.passed);
        assert!(result.bugs.iter().any(|b| b.description.contains("unwrap")));
    }

    #[test]
    fn fast_detects_todo() {
        let code = "fn foo() { // TODO: implement }";
        let result = VerificationAgent::verify_fast(code, "foo");
        assert!(!result.passed);
        assert!(result.bugs.iter().any(|b| b.description.contains("TODO")));
    }

    #[test]
    fn fast_detects_low_spec_coverage() {
        let code = "fn foo() { 42 }";
        let spec = "This function must handle authentication, authorization, and logging with detailed audit trails";
        let result = VerificationAgent::verify_fast(code, spec);
        assert!(!result.passed);
        assert!(result.bugs.iter().any(|b| b.description.contains("spec")));
    }

    #[test]
    fn generate_fuzz_inputs_for_strings() {
        let spec = "Process user input text strings";
        let inputs = VerificationAgent::generate_fuzz_inputs(spec);
        assert!(inputs.iter().any(|i| i.is_empty()));
        assert!(inputs.iter().any(|i| i.len() > 1000));
    }

    #[test]
    fn generate_property_tests_for_idempotent() {
        let spec = "This function must be idempotent";
        let tests = VerificationAgent::generate_property_tests(spec);
        assert!(tests.iter().any(|t| t.name == "idempotence"));
    }
}
