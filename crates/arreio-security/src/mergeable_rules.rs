//! Regras mergeaveis de permissao (GAP-011).
//!
//! Regras podem vir de quatro escopos. O merge concatena listas e preserva
//! informacao de escopo para que o avaliador escolha a regra mais especifica.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleScope {
    Managed,
    User,
    Project,
    Local,
}

impl RuleScope {
    pub fn precedence(self) -> u8 {
        match self {
            RuleScope::Managed => 0,
            RuleScope::User => 1,
            RuleScope::Project => 2,
            RuleScope::Local => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    pub pattern: Option<String>,
    pub scope: RuleScope,
}

impl PermissionRule {
    pub fn parse(input: &str, scope: RuleScope) -> Option<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(open) = trimmed.find('(') {
            if !trimmed.ends_with(')') || open == 0 {
                return None;
            }
            let tool = trimmed[..open].trim();
            let pattern = trimmed[open + 1..trimmed.len() - 1].trim();
            if tool.is_empty() || pattern.is_empty() {
                return None;
            }
            return Some(Self {
                tool: tool.to_string(),
                pattern: Some(pattern.to_string()),
                scope,
            });
        }

        Some(Self {
            tool: trimmed.to_string(),
            pattern: None,
            scope,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRules {
    pub allow: Vec<PermissionRule>,
    pub ask: Vec<PermissionRule>,
    pub deny: Vec<PermissionRule>,
}

impl PermissionRules {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleDecision {
    Allow,
    Ask,
    Deny,
}

pub struct RuleMerger;

impl RuleMerger {
    pub fn merge(scoped_rules: Vec<PermissionRules>) -> PermissionRules {
        let mut merged = PermissionRules::new();
        for rules in scoped_rules {
            merged.allow.extend(rules.allow);
            merged.ask.extend(rules.ask);
            merged.deny.extend(rules.deny);
        }
        merged
    }

    pub fn decide(
        rules: &PermissionRules,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Option<RuleDecision> {
        let mut best: Option<(RuleDecision, u8, u8)> = None;

        for (decision, entries) in [
            (RuleDecision::Allow, rules.allow.as_slice()),
            (RuleDecision::Ask, rules.ask.as_slice()),
            (RuleDecision::Deny, rules.deny.as_slice()),
        ] {
            for rule in entries {
                if !rule.matches(tool_name, arguments) {
                    continue;
                }
                let score = rule.specificity_score();
                let rank = decision_rank(decision);
                match best {
                    None => best = Some((decision, score, rank)),
                    Some((_, best_score, best_rank))
                        if score > best_score || (score == best_score && rank > best_rank) =>
                    {
                        best = Some((decision, score, rank));
                    }
                    _ => {}
                }
            }
        }

        best.map(|(decision, _, _)| decision)
    }
}

impl PermissionRule {
    fn specificity_score(&self) -> u8 {
        self.scope.precedence() * 2 + u8::from(self.pattern.is_some())
    }

    fn matches(&self, tool_name: &str, arguments: &serde_json::Value) -> bool {
        if self.tool != tool_name {
            return false;
        }

        match self.pattern.as_deref() {
            None => true,
            Some(pattern) => {
                let haystacks = [
                    arguments.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                    arguments.get("cwd").and_then(|v| v.as_str()).unwrap_or(""),
                    arguments
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    arguments
                        .get("query")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                ];
                haystacks.iter().any(|value| value.contains(pattern))
                    || serde_json::to_string(arguments)
                        .map(|text| text.contains(pattern))
                        .unwrap_or(false)
            }
        }
    }
}

fn decision_rank(decision: RuleDecision) -> u8 {
    match decision {
        RuleDecision::Allow => 0,
        RuleDecision::Ask => 1,
        RuleDecision::Deny => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_blanket_rule_without_pattern() {
        let rule = PermissionRule::parse("read_file", RuleScope::Project).unwrap();
        assert_eq!(rule.tool, "read_file");
        assert_eq!(rule.pattern, None);
        assert_eq!(rule.scope, RuleScope::Project);
    }

    #[test]
    fn parse_pattern_rule() {
        let rule = PermissionRule::parse("write_file(src/)", RuleScope::Local).unwrap();
        assert_eq!(rule.tool, "write_file");
        assert_eq!(rule.pattern.as_deref(), Some("src/"));
        assert_eq!(rule.scope, RuleScope::Local);
    }

    #[test]
    fn merge_concatenates_all_lists() {
        let managed = PermissionRules {
            deny: vec![PermissionRule::parse("exec", RuleScope::Managed).unwrap()],
            ..PermissionRules::new()
        };
        let project = PermissionRules {
            allow: vec![PermissionRule::parse("read_file", RuleScope::Project).unwrap()],
            ask: vec![PermissionRule::parse("write_file", RuleScope::Project).unwrap()],
            ..PermissionRules::new()
        };

        let merged = RuleMerger::merge(vec![managed, project]);
        assert_eq!(merged.deny.len(), 1);
        assert_eq!(merged.allow.len(), 1);
        assert_eq!(merged.ask.len(), 1);
    }

    #[test]
    fn deny_wins_over_allow_for_same_specificity() {
        let rules = PermissionRules {
            allow: vec![PermissionRule::parse("exec", RuleScope::Project).unwrap()],
            deny: vec![PermissionRule::parse("exec", RuleScope::Project).unwrap()],
            ..PermissionRules::new()
        };

        let decision = RuleMerger::decide(&rules, "exec", &serde_json::json!({}));
        assert_eq!(decision, Some(RuleDecision::Deny));
    }

    #[test]
    fn local_specific_rule_wins_over_project_blanket() {
        let rules = PermissionRules {
            deny: vec![PermissionRule::parse("write_file", RuleScope::Project).unwrap()],
            allow: vec![PermissionRule::parse("write_file(src/)", RuleScope::Local).unwrap()],
            ..PermissionRules::new()
        };

        let decision = RuleMerger::decide(
            &rules,
            "write_file",
            &serde_json::json!({"path": "src/lib.rs"}),
        );
        assert_eq!(decision, Some(RuleDecision::Allow));
    }

    #[test]
    fn malformed_rules_are_rejected() {
        assert!(PermissionRule::parse("", RuleScope::User).is_none());
        assert!(PermissionRule::parse("(src/)", RuleScope::User).is_none());
        assert!(PermissionRule::parse("write_file(", RuleScope::User).is_none());
        assert!(PermissionRule::parse("write_file()", RuleScope::User).is_none());
    }
}
