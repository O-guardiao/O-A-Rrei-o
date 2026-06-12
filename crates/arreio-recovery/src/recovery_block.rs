//! Bloco de recuperação principal — tolerância a falhas via diversidade de LLMs.
//!
//! Sintaxe conceitual: ensure <acceptance_test> by <primary> else by <alt1> else by <alt2> else error
//!
//! Referências:
//! - Randell, B. (1974). "System Structure for Software Fault Tolerance"
//! - Avizienis, A. (1985). "The N-Version Approach to Fault-Tolerant Software"

use anyhow::Result;

use crate::{AcceptanceTest, ModelVariant, RecoveryCache};

/// Log de uma tentativa de execução com um modelo específico.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptLog {
    /// Nome do modelo utilizado nesta tentativa.
    pub model_name: String,
    /// Indica se o acceptance test passou para esta tentativa.
    pub passed: bool,
    /// Saída bruta (ou mensagem de erro) da tentativa.
    pub output: String,
}

/// Resultado consolidado da execução de um recovery block.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryResult {
    /// Indica se alguma tentativa obteve sucesso no acceptance test.
    pub success: bool,
    /// Saída da tentativa bem-sucedida (vazia se todas falharam).
    pub output: String,
    /// Logs de todas as tentativas realizadas, em ordem.
    pub attempts: Vec<AttemptLog>,
}

/// Bloco de recuperação que orquestra execução primária e alternativas.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryBlock {
    /// Variante primária de modelo.
    pub primary: ModelVariant,
    /// Variantes alternativas, em ordem de prioridade.
    pub alternates: Vec<ModelVariant>,
    /// Teste de aceitação que valida cada resultado.
    pub acceptance: AcceptanceTest,
    /// Cache para salvamento/restauração de estado entre tentativas.
    pub cache: RecoveryCache,
}

impl RecoveryBlock {
    /// Executa o recovery block sobre uma tarefa.
    ///
    /// A função `executor` recebe (`&ModelVariant`, `task`) e retorna `Result<String>`.
    /// A lógica é:
    /// 1. Executa o primário → acceptance test → sucesso retorna.
    /// 2. Em falha, tenta restaurar estado do cache e executa alternativas em sequência.
    /// 3. Se todas falharem, retorna `RecoveryResult` com `success: false`.
    pub fn execute<F>(&self, task: &str, executor: F) -> Result<RecoveryResult>
    where
        F: Fn(&ModelVariant, &str) -> Result<String>,
    {
        let mut attempts = Vec::new();

        // --- Tentativa primária ---
        match executor(&self.primary, task) {
            Ok(output) => {
                let passed = self.acceptance.evaluate(&output);
                attempts.push(AttemptLog {
                    model_name: self.primary.name.clone(),
                    passed,
                    output: output.clone(),
                });
                if passed {
                    return Ok(RecoveryResult {
                        success: true,
                        output,
                        attempts,
                    });
                }
            }
            Err(e) => {
                attempts.push(AttemptLog {
                    model_name: self.primary.name.clone(),
                    passed: false,
                    output: format!("erro: {}", e),
                });
            }
        }

        // --- Tentativas alternativas com restauração de estado ---
        for alt in &self.alternates {
            // Tenta restaurar estado previamente salvo antes da execução.
            // Falhas de restore são ignoradas (pode ser primeira execução ou cache vazio).
            let _ = self.cache.restore_state("pre_execution");

            match executor(alt, task) {
                Ok(output) => {
                    let passed = self.acceptance.evaluate(&output);
                    attempts.push(AttemptLog {
                        model_name: alt.name.clone(),
                        passed,
                        output: output.clone(),
                    });
                    if passed {
                        return Ok(RecoveryResult {
                            success: true,
                            output,
                            attempts,
                        });
                    }
                }
                Err(e) => {
                    attempts.push(AttemptLog {
                        model_name: alt.name.clone(),
                        passed: false,
                        output: format!("erro: {}", e),
                    });
                }
            }
        }

        Ok(RecoveryResult {
            success: false,
            output: String::new(),
            attempts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AcceptanceTest, ModelVariant, Predicate, RecoveryCache};
    use serde_json::json;
    use tempfile::TempDir;

    fn make_cache() -> RecoveryCache {
        let temp_dir = TempDir::new().unwrap();
        RecoveryCache::new(temp_dir.path().to_path_buf())
    }

    #[test]
    fn test_recovery_block_primary_passes() {
        let block = RecoveryBlock {
            primary: ModelVariant {
                name: "primary".to_string(),
                provider_config: json!({}),
            },
            alternates: vec![],
            acceptance: AcceptanceTest {
                input_tests: vec![],
                output_tests: vec![Predicate::NonEmpty],
                integrity_tests: vec![],
            },
            cache: make_cache(),
        };

        let result = block
            .execute("task", |_model, _task| Ok("output".to_string()))
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "output");
        assert_eq!(result.attempts.len(), 1);
        assert!(result.attempts[0].passed);
    }

    #[test]
    fn test_recovery_block_primary_fails_alternate_passes() {
        let block = RecoveryBlock {
            primary: ModelVariant {
                name: "primary".to_string(),
                provider_config: json!({}),
            },
            alternates: vec![ModelVariant {
                name: "alt1".to_string(),
                provider_config: json!({}),
            }],
            acceptance: AcceptanceTest {
                input_tests: vec![],
                output_tests: vec![Predicate::Contains("success".to_string())],
                integrity_tests: vec![],
            },
            cache: make_cache(),
        };

        let result = block
            .execute("task", |model, _task| {
                if model.name == "primary" {
                    Ok("failure".to_string())
                } else {
                    Ok("success output".to_string())
                }
            })
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "success output");
        assert_eq!(result.attempts.len(), 2);
        assert!(!result.attempts[0].passed);
        assert!(result.attempts[1].passed);
    }

    #[test]
    fn test_recovery_block_all_fail() {
        let block = RecoveryBlock {
            primary: ModelVariant {
                name: "primary".to_string(),
                provider_config: json!({}),
            },
            alternates: vec![ModelVariant {
                name: "alt1".to_string(),
                provider_config: json!({}),
            }],
            acceptance: AcceptanceTest {
                input_tests: vec![],
                output_tests: vec![Predicate::Eq("exact".to_string())],
                integrity_tests: vec![],
            },
            cache: make_cache(),
        };

        let result = block
            .execute("task", |_model, _task| Ok("wrong".to_string()))
            .unwrap();
        assert!(!result.success);
        assert_eq!(result.output, "");
        assert_eq!(result.attempts.len(), 2);
        assert!(!result.attempts[0].passed);
        assert!(!result.attempts[1].passed);
    }

    #[test]
    fn test_recovery_block_executor_error_counts_as_fail() {
        let block = RecoveryBlock {
            primary: ModelVariant {
                name: "primary".to_string(),
                provider_config: json!({}),
            },
            alternates: vec![],
            acceptance: AcceptanceTest {
                input_tests: vec![],
                output_tests: vec![Predicate::NonEmpty],
                integrity_tests: vec![],
            },
            cache: make_cache(),
        };

        let result = block
            .execute("task", |_model, _task| anyhow::bail!("simulated error"))
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.attempts.len(), 1);
        assert!(!result.attempts[0].passed);
        assert!(result.attempts[0].output.contains("erro:"));
    }

    #[test]
    fn test_recovery_result_counts_attempts() {
        let block = RecoveryBlock {
            primary: ModelVariant {
                name: "p".to_string(),
                provider_config: json!({}),
            },
            alternates: vec![
                ModelVariant {
                    name: "a1".to_string(),
                    provider_config: json!({}),
                },
                ModelVariant {
                    name: "a2".to_string(),
                    provider_config: json!({}),
                },
            ],
            acceptance: AcceptanceTest {
                input_tests: vec![],
                output_tests: vec![Predicate::Contains("x".to_string())],
                integrity_tests: vec![],
            },
            cache: make_cache(),
        };

        let result = block
            .execute("task", |model, _task| {
                if model.name == "a2" {
                    Ok("has x".to_string())
                } else {
                    Ok("no".to_string())
                }
            })
            .unwrap();

        assert!(result.success);
        assert_eq!(result.attempts.len(), 3);
        assert_eq!(result.attempts[0].model_name, "p");
        assert_eq!(result.attempts[1].model_name, "a1");
        assert_eq!(result.attempts[2].model_name, "a2");
        assert!(result.attempts[2].passed);
    }
}
