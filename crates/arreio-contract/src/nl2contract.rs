use crate::contract::{Contract, Predicate, PredicateEvaluator};
use anyhow::Result;
use regex::Regex;

/// Parser simples de linguagem natural para contrato.
pub struct NL2Contract;

impl NL2Contract {
    /// Analisa uma especificação em linguagem natural e constrói um `Contract`.
    ///
    /// Espera linhas no formato:
    /// - `precondition: <descrição>`
    /// - `postcondition: <descrição>`
    /// - `invariant: <descrição>`
    /// - `name: <nome do contrato>`
    pub fn parse(nl_spec: &str) -> Result<Contract> {
        let mut name = String::from("unnamed");
        let mut preconditions = Vec::new();
        let mut postconditions = Vec::new();
        let mut invariants = Vec::new();

        let pre_re = Regex::new(r"(?i)^\s*precondition\s*[:\-]\s*(.+)$")?;
        let post_re = Regex::new(r"(?i)^\s*postcondition\s*[:\-]\s*(.+)$")?;
        let inv_re = Regex::new(r"(?i)^\s*invariant\s*[:\-]\s*(.+)$")?;
        let name_re = Regex::new(r"(?i)^\s*name\s*[:\-]\s*(.+)$")?;

        for line in nl_spec.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(caps) = name_re.captures(trimmed) {
                name = caps[1].trim().to_string();
            } else if let Some(caps) = pre_re.captures(trimmed) {
                let desc = caps[1].trim().to_string();
                preconditions.push(Predicate {
                    id: format!("pre_{}", preconditions.len()),
                    description: desc.clone(),
                    expression: desc.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Avalie se a seguinte pré-condição é satisfeita: {}", desc),
                    },
                });
            } else if let Some(caps) = post_re.captures(trimmed) {
                let desc = caps[1].trim().to_string();
                postconditions.push(Predicate {
                    id: format!("post_{}", postconditions.len()),
                    description: desc.clone(),
                    expression: desc.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Avalie se a seguinte pós-condição é satisfeita: {}", desc),
                    },
                });
            } else if let Some(caps) = inv_re.captures(trimmed) {
                let desc = caps[1].trim().to_string();
                invariants.push(Predicate {
                    id: format!("inv_{}", invariants.len()),
                    description: desc.clone(),
                    expression: desc.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Avalie se a seguinte invariante é mantida: {}", desc),
                    },
                });
            }
        }

        if name == "unnamed"
            && preconditions.is_empty()
            && postconditions.is_empty()
            && invariants.is_empty()
        {
            return Err(anyhow::anyhow!(
                "Nenhuma condição de contrato encontrada na especificação"
            ));
        }

        Ok(Contract {
            name,
            preconditions,
            postconditions,
            invariants,
        })
    }

    /// Extrai predicados de um docstring no estilo Rust (`/// precondition: ...`).
    pub fn extract_from_docstring(doc: &str) -> Vec<Predicate> {
        let mut predicates = Vec::new();
        let pre_re = Regex::new(r"(?i)precondition\s*[:\-]\s*(.+)").unwrap();
        let post_re = Regex::new(r"(?i)postcondition\s*[:\-]\s*(.+)").unwrap();
        let inv_re = Regex::new(r"(?i)invariant\s*[:\-]\s*(.+)").unwrap();

        for line in doc.lines() {
            let trimmed = line
                .trim_start()
                .trim_start_matches("///")
                .trim_start_matches("//")
                .trim_start_matches("/*")
                .trim_start_matches("*")
                .trim();

            if let Some(caps) = pre_re.captures(trimmed) {
                let desc = caps[1].trim().to_string();
                predicates.push(Predicate {
                    id: format!("doc_pre_{}", predicates.len()),
                    description: desc.clone(),
                    expression: desc.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Avalie pré-condição: {}", desc),
                    },
                });
            } else if let Some(caps) = post_re.captures(trimmed) {
                let desc = caps[1].trim().to_string();
                predicates.push(Predicate {
                    id: format!("doc_post_{}", predicates.len()),
                    description: desc.clone(),
                    expression: desc.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Avalie pós-condição: {}", desc),
                    },
                });
            } else if let Some(caps) = inv_re.captures(trimmed) {
                let desc = caps[1].trim().to_string();
                predicates.push(Predicate {
                    id: format!("doc_inv_{}", predicates.len()),
                    description: desc.clone(),
                    expression: desc.clone(),
                    evaluator: PredicateEvaluator::LlmEvaluated {
                        prompt: format!("Avalie invariante: {}", desc),
                    },
                });
            }
        }

        predicates
    }
}
