//! Diversidade de modelos — pool de variantes de LLM para recovery blocks.
//!
//! Permite alternar entre diferentes modelos/configurações quando o primário falha,
//! explorando diversidade de comportamento (Randell 1974, Avizienis 1985).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Variante de modelo LLM com sua configuração de provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelVariant {
    /// Nome identificador da variante (ex: "gemma4:latest", "llama3:8b").
    pub name: String,
    /// Configuração específica do provider em JSON livre.
    pub provider_config: Value,
}

/// Pool de variantes disponíveis para execução diversificada.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelPool {
    /// Lista ordenada de variantes de modelo.
    pub variants: Vec<ModelVariant>,
}

impl ModelPool {
    /// Retorna a próxima variante ainda não exaustada.
    /// A ordem de prioridade é a ordem do vetor `variants`.
    pub fn next_variant(&mut self, exhausted: &[String]) -> Option<ModelVariant> {
        self.variants
            .iter()
            .find(|v| !exhausted.contains(&v.name))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_model_pool_rotates_variants() {
        let mut pool = ModelPool {
            variants: vec![
                ModelVariant {
                    name: "alpha".to_string(),
                    provider_config: json!({}),
                },
                ModelVariant {
                    name: "beta".to_string(),
                    provider_config: json!({}),
                },
            ],
        };

        let v1 = pool.next_variant(&[]).unwrap();
        assert_eq!(v1.name, "alpha");

        let v2 = pool.next_variant(&["alpha".to_string()]).unwrap();
        assert_eq!(v2.name, "beta");
    }

    #[test]
    fn test_model_pool_exhaustion_returns_none() {
        let mut pool = ModelPool {
            variants: vec![ModelVariant {
                name: "alpha".to_string(),
                provider_config: json!({}),
            }],
        };

        let v = pool.next_variant(&["alpha".to_string()]);
        assert!(v.is_none());
    }

    #[test]
    fn test_model_pool_empty() {
        let mut pool = ModelPool { variants: vec![] };
        assert!(pool.next_variant(&[]).is_none());
    }

    #[test]
    fn test_model_pool_skips_exhausted() {
        let mut pool = ModelPool {
            variants: vec![
                ModelVariant {
                    name: "alpha".to_string(),
                    provider_config: json!({}),
                },
                ModelVariant {
                    name: "beta".to_string(),
                    provider_config: json!({}),
                },
                ModelVariant {
                    name: "gamma".to_string(),
                    provider_config: json!({}),
                },
            ],
        };

        let v = pool
            .next_variant(&["alpha".to_string(), "beta".to_string()])
            .unwrap();
        assert_eq!(v.name, "gamma");
    }
}
