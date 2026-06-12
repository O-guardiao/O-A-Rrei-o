//! Escalation Policies — Regras declarativas de aprovação humana (PVC-Q1.2).
//!
//! Policies são definidas em `~/.arreio/rules/escalation.yaml` e parseadas em runtime
//! pelo `EscalationEngine`. Cada policy especifica triggers (tool, custo, violação
//! de contrato) e ação (require_approval, auto_reject, log_only).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ── Tipos Públicos ────────────────────────────────────────────────────────────

/// Uma escalation policy individual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationPolicy {
    pub name: String,
    pub triggers: Vec<PolicyTrigger>,
    pub action: PolicyAction,
    pub approvers: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
    #[serde(default)]
    pub escalation_target: Option<String>,
}

fn default_timeout() -> u64 {
    300 // 5 minutos padrão
}

/// Trigger que dispara uma policy.
///
/// Formato YAML: campos opcionais — preencha apenas o campo relevante.
/// ```yaml
/// triggers:
///   - tool: "db_delete"
///   - cost_estimate_usd: "> 100.00"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyTrigger {
    pub tool: Option<String>,
    pub pattern: Option<String>,
    pub contract_violation: Option<String>,
    pub cost_estimate_usd: Option<String>,
    pub dag_node_tag: Option<String>,
}

/// Ação a ser tomada quando uma policy dispara.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    RequireApproval,
    AutoReject,
    LogOnly,
}

/// Conjunto de policies carregadas de um arquivo YAML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationPolicySet {
    pub version: String,
    pub policies: Vec<EscalationPolicy>,
}

/// Resultado da avaliação de uma policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyMatch {
    pub policy_name: String,
    pub action: PolicyAction,
    pub approvers: Vec<String>,
    pub timeout_sec: u64,
    pub escalation_target: Option<String>,
}

/// Contexto de avaliação — informações sobre a ação sendo verificada.
#[derive(Debug, Clone, Default)]
pub struct EvaluationContext {
    pub tool_name: String,
    pub cost_estimate_usd: Option<f64>,
    pub contract_violation_severity: Option<String>,
    pub dag_node_tag: Option<String>,
}

// ── Escalation Engine ─────────────────────────────────────────────────────────

/// Motor de parse e avaliação de escalation policies.
pub struct EscalationEngine {
    policies: Vec<EscalationPolicy>,
    last_loaded: u64,
    cache_ttl_sec: u64,
    source_path: Option<std::path::PathBuf>,
}

impl EscalationEngine {
    /// Cria engine vazio (sem policies).
    pub fn empty() -> Self {
        Self {
            policies: vec![],
            last_loaded: 0,
            cache_ttl_sec: 30,
            source_path: None,
        }
    }

    /// Carrega policies de um arquivo YAML.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("lendo escalation policies em {}", path.display()))?;
        let set: EscalationPolicySet = serde_yaml::from_str(&raw)
            .with_context(|| format!("parse YAML de escalation policies em {}", path.display()))?;

        Ok(Self {
            policies: set.policies,
            last_loaded: now(),
            cache_ttl_sec: 30,
            source_path: Some(path.to_path_buf()),
        })
    }

    /// Recarrega policies se o cache expirou e o arquivo existe.
    pub fn reload_if_stale(&mut self) -> Result<()> {
        let now = now();
        if now - self.last_loaded < self.cache_ttl_sec {
            return Ok(());
        }
        if let Some(ref path) = self.source_path {
            if path.exists() {
                *self = Self::load(path)?;
            }
        }
        Ok(())
    }

    /// Avalia todas as policies contra um contexto e retorna matches ordenados
    /// por especificidade (mais específicos primeiro).
    pub fn evaluate(&mut self, ctx: &EvaluationContext) -> Result<Vec<PolicyMatch>> {
        self.reload_if_stale()?;

        let mut matches = Vec::new();
        for policy in &self.policies {
            if Self::policy_matches(policy, ctx) {
                matches.push(PolicyMatch {
                    policy_name: policy.name.clone(),
                    action: policy.action,
                    approvers: policy.approvers.clone(),
                    timeout_sec: policy.timeout_sec,
                    escalation_target: policy.escalation_target.clone(),
                });
            }
        }

        // Ordena por especificidade: tool exato > pattern > tag > violation > genérico
        matches.sort_by_key(|m| Self::specificity_score(&m.policy_name));
        matches.reverse();

        Ok(matches)
    }

    /// Retorna todas as policies carregadas (para inspeção/debug).
    pub fn policies(&self) -> &[EscalationPolicy] {
        &self.policies
    }

    // ── Internals ─────────────────────────────────────────────────────────────

    fn policy_matches(policy: &EscalationPolicy, ctx: &EvaluationContext) -> bool {
        policy.triggers.iter().any(|trigger| {
            // Tool name exact match
            if let Some(ref tool) = trigger.tool {
                if ctx.tool_name == *tool {
                    return true;
                }
            }
            // Pattern match (substring or wildcard)
            if let Some(ref pattern) = trigger.pattern {
                if ctx.tool_name.contains(pattern) || pattern == "*" {
                    return true;
                }
            }
            // Contract violation severity
            if let Some(ref severity) = trigger.contract_violation {
                if ctx.contract_violation_severity
                    .as_ref()
                    .map(|s| s == severity)
                    .unwrap_or(false)
                {
                    return true;
                }
            }
            // Cost estimate threshold
            if let Some(ref threshold) = trigger.cost_estimate_usd {
                if Self::cost_matches(ctx.cost_estimate_usd, threshold) {
                    return true;
                }
            }
            // DAG node tag
            if let Some(ref tag) = trigger.dag_node_tag {
                if ctx.dag_node_tag.as_ref().map(|t| t == tag).unwrap_or(false) {
                    return true;
                }
            }
            false
        })
    }

    fn cost_matches(cost: Option<f64>, threshold: &str) -> bool {
        let Some(cost) = cost else { return false };
        let threshold = threshold.trim();
        if let Some(t) = threshold.strip_prefix("gt ") {
            if let Ok(v) = t.parse::<f64>() {
                return cost > v;
            }
        }
        if let Some(t) = threshold.strip_prefix("gte ") {
            if let Ok(v) = t.parse::<f64>() {
                return cost >= v;
            }
        }
        if let Some(t) = threshold.strip_prefix("lt ") {
            if let Ok(v) = t.parse::<f64>() {
                return cost < v;
            }
        }
        if let Some(t) = threshold.strip_prefix("lte ") {
            if let Ok(v) = t.parse::<f64>() {
                return cost <= v;
            }
        }
        if let Ok(v) = threshold.parse::<f64>() {
            return cost >= v;
        }
        false
    }

    fn specificity_score(policy_name: &str) -> u8 {
        // Heurística simples: nomes mais descritivos = mais específicos
        // Em produção, usaríamos metadados da policy
        match policy_name {
            n if n.contains("tool") || n.contains("specific") => 5,
            n if n.contains("pattern") || n.contains("tag") => 4,
            n if n.contains("cost") || n.contains("financial") => 3,
            n if n.contains("violation") || n.contains("severity") => 2,
            _ => 1,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn escalation_engine_matches_tool_name() {
        let yaml = r#"
version: "1.0"
policies:
  - name: "db_delete_guard"
    triggers:
      - tool: "db_delete"
    action: "require_approval"
    approvers: ["data_owner"]
    timeout_sec: 600
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let mut engine = EscalationEngine::load(tmp.path()).unwrap();
        let ctx = EvaluationContext {
            tool_name: "db_delete".into(),
            ..Default::default()
        };
        let matches = engine.evaluate(&ctx).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].policy_name, "db_delete_guard");
        assert_eq!(matches[0].action, PolicyAction::RequireApproval);
    }

    #[test]
    fn escalation_engine_returns_empty_for_safe_tool() {
        let yaml = r#"
version: "1.0"
policies:
  - name: "db_delete_guard"
    triggers:
      - tool_name:
          tool: "db_delete"
    action: "require_approval"
    approvers: ["data_owner"]
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let mut engine = EscalationEngine::load(tmp.path()).unwrap();
        let ctx = EvaluationContext {
            tool_name: "read_file".into(),
            ..Default::default()
        };
        let matches = engine.evaluate(&ctx).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn escalation_engine_matches_cost_threshold() {
        let yaml = r#"
version: "1.0"
policies:
  - name: "expensive_action"
    triggers:
      - cost_estimate_usd: "gt 100.00"
    action: "require_approval"
    approvers: ["finance_owner"]
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let mut engine = EscalationEngine::load(tmp.path()).unwrap();
        let ctx = EvaluationContext {
            tool_name: "payment_execute".into(),
            cost_estimate_usd: Some(150.0),
            ..Default::default()
        };
        let matches = engine.evaluate(&ctx).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].policy_name, "expensive_action");
    }

    #[test]
    fn escalation_engine_ignores_cost_below_threshold() {
        let yaml = r#"
version: "1.0"
policies:
  - name: "expensive_action"
    triggers:
      - cost_estimate_usd: "gt 100.00"
    action: "require_approval"
    approvers: ["finance_owner"]
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let mut engine = EscalationEngine::load(tmp.path()).unwrap();
        let ctx = EvaluationContext {
            tool_name: "payment_execute".into(),
            cost_estimate_usd: Some(50.0),
            ..Default::default()
        };
        let matches = engine.evaluate(&ctx).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn escalation_engine_matches_contract_violation() {
        let yaml = r#"
version: "1.0"
policies:
  - name: "severe_violation"
    triggers:
      - contract_violation: "Critical"
    action: "require_approval"
    approvers: ["admin"]
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let mut engine = EscalationEngine::load(tmp.path()).unwrap();
        let ctx = EvaluationContext {
            tool_name: "any_tool".into(),
            contract_violation_severity: Some("Critical".into()),
            ..Default::default()
        };
        let matches = engine.evaluate(&ctx).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].policy_name, "severe_violation");
    }

    #[test]
    fn escalation_engine_auto_reject_policy() {
        let yaml = r#"
version: "1.0"
policies:
  - name: "rm_rf_guard"
    triggers:
      - tool: "rm_rf"
    action: "auto_reject"
    approvers: []
"#;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let mut engine = EscalationEngine::load(tmp.path()).unwrap();
        let ctx = EvaluationContext {
            tool_name: "rm_rf".into(),
            ..Default::default()
        };
        let matches = engine.evaluate(&ctx).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].action, PolicyAction::AutoReject);
    }

    #[test]
    fn escalation_engine_empty_when_no_file() {
        let engine = EscalationEngine::empty();
        let _ctx = EvaluationContext {
            tool_name: "anything".into(),
            ..Default::default()
        };
        // Engine vazio não tem policies carregadas
        assert!(engine.policies().is_empty());
    }

    #[test]
    fn escalation_engine_parse_invalid_yaml_falls_back() {
        let yaml = "not_valid::: yaml{{{";
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(yaml.as_bytes()).unwrap();

        let result = EscalationEngine::load(tmp.path());
        assert!(result.is_err()); // Deve falhar graciosamente
    }

    #[test]
    fn cost_matches_various_operators() {
        assert!(EscalationEngine::cost_matches(Some(150.0), "gt 100.00"));
        assert!(!EscalationEngine::cost_matches(Some(50.0), "gt 100.00"));
        assert!(EscalationEngine::cost_matches(Some(100.0), "gte 100.00"));
        assert!(EscalationEngine::cost_matches(Some(99.0), "lt 100.00"));
        assert!(EscalationEngine::cost_matches(Some(100.0), "lte 100.00"));
        assert!(EscalationEngine::cost_matches(Some(100.0), "100.00"));
        assert!(!EscalationEngine::cost_matches(None, "gt 100.00"));
    }
}
