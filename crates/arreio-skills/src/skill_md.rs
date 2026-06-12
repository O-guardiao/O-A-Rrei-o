use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Skill com YAML frontmatter (formato SKILL.md).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMd {
    pub name: String,
    pub description: String,
    pub version: String,
    pub license: Option<String>,
    pub platforms: Vec<String>,
    pub prerequisites: Vec<String>,
    pub tags: Vec<String>,
    pub related_skills: Vec<String>,
    pub when_to_use: String,
    pub when_not_to_use: String,
    pub quick_reference: String,
    pub examples: String,
    pub body: String,
    // ── Regras de Ouro & Insights (Harness Pattern + SkillsBench) ──
    pub error_budget: u32,
    pub output_schema: Option<String>,
    pub allowed_tools: Vec<String>,
    pub anti_conversation: bool,
    pub idempotent: bool,
    pub trust_level: String, // "untrusted" | "validated" | "trusted"
    pub module_count: u32,
}

impl SkillMd {
    pub fn parse(content: &str) -> Result<Self> {
        let content = content.trim();
        if !content.starts_with("---") {
            bail!("SKILL.md deve começar com YAML frontmatter (---)");
        }

        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            bail!("frontmatter malformado: esperado --- ... ---");
        }

        let frontmatter = parts[1].trim();
        let body = parts[2].trim();

        let mut name = String::new();
        let mut description = String::new();
        let mut version = "0.1.0".to_string();
        let mut license = None;
        let mut platforms = Vec::new();
        let mut prerequisites = Vec::new();
        let mut tags = Vec::new();
        let mut related_skills = Vec::new();
        // ── Regras de Ouro & Insights ──
        let mut error_budget: u32 = 3;
        let mut output_schema: Option<String> = None;
        let mut allowed_tools = Vec::new();
        let mut anti_conversation = true;
        let mut idempotent = false;
        let mut trust_level = "untrusted".to_string();
        let mut module_count: u32 = 1;

        for line in frontmatter.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim().trim_matches('"').trim_matches('\'');
                match key {
                    "name" => name = value.to_string(),
                    "description" => description = value.to_string(),
                    "version" => version = value.to_string(),
                    "license" => license = Some(value.to_string()),
                    "platforms" => platforms = parse_list(value),
                    "prerequisites" => prerequisites = parse_list(value),
                    "tags" => tags = parse_list(value),
                    "related_skills" => related_skills = parse_list(value),
                    // ── Regras de Ouro & Insights ──
                    "error_budget" => {
                        if let Ok(n) = value.parse::<u32>() {
                            error_budget = n;
                        }
                    }
                    "output_schema" => output_schema = Some(value.to_string()),
                    "allowed_tools" => allowed_tools = parse_list(value),
                    "anti_conversation" => anti_conversation = value == "true",
                    "idempotent" => idempotent = value == "true",
                    "trust_level" => trust_level = value.to_string(),
                    "module_count" => {
                        if let Ok(n) = value.parse::<u32>() {
                            module_count = n;
                        }
                    }
                    _ => {}
                }
            }
        }

        if name.is_empty() {
            bail!("campo 'name' é obrigatório no frontmatter");
        }

        // Extrai seções do corpo
        let mut when_to_use = String::new();
        let mut when_not_to_use = String::new();
        let mut quick_reference = String::new();
        let mut examples = String::new();

        let mut current_section = "";
        let mut current_content = String::new();

        for line in body.lines() {
            if line.starts_with("## ") || line.starts_with("### ") {
                // Flush seção anterior
                match current_section {
                    "When to Use" => when_to_use = current_content.trim().to_string(),
                    "When NOT to Use" => when_not_to_use = current_content.trim().to_string(),
                    "Quick Reference" => quick_reference = current_content.trim().to_string(),
                    "Examples" => examples = current_content.trim().to_string(),
                    _ => {}
                }
                current_section = line
                    .trim_start_matches("## ")
                    .trim_start_matches("### ")
                    .trim();
                current_content = String::new();
            } else {
                current_content.push_str(line);
                current_content.push('\n');
            }
        }
        // Flush última seção
        match current_section {
            "When to Use" => when_to_use = current_content.trim().to_string(),
            "When NOT to Use" => when_not_to_use = current_content.trim().to_string(),
            "Quick Reference" => quick_reference = current_content.trim().to_string(),
            "Examples" => examples = current_content.trim().to_string(),
            _ => {}
        }

        Ok(SkillMd {
            name,
            description,
            version,
            license,
            platforms,
            prerequisites,
            tags,
            related_skills,
            when_to_use,
            when_not_to_use,
            quick_reference,
            examples,
            body: body.to_string(),
            // ── Regras de Ouro & Insights ──
            error_budget,
            output_schema,
            allowed_tools,
            anti_conversation,
            idempotent,
            trust_level,
            module_count,
        })
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("name: {}\n", self.name));
        out.push_str(&format!("description: {}\n", self.description));
        out.push_str(&format!("version: {}\n", self.version));
        if let Some(ref lic) = self.license {
            out.push_str(&format!("license: {}\n", lic));
        }
        if !self.platforms.is_empty() {
            out.push_str(&format!("platforms: {}\n", self.platforms.join(", ")));
        }
        if !self.prerequisites.is_empty() {
            out.push_str(&format!(
                "prerequisites: {}\n",
                self.prerequisites.join(", ")
            ));
        }
        if !self.tags.is_empty() {
            out.push_str(&format!("tags: {}\n", self.tags.join(", ")));
        }
        if !self.related_skills.is_empty() {
            out.push_str(&format!(
                "related_skills: {}\n",
                self.related_skills.join(", ")
            ));
        }
        // ── Regras de Ouro & Insights ──
        out.push_str(&format!("error_budget: {}\n", self.error_budget));
        out.push_str(&format!("anti_conversation: {}\n", self.anti_conversation));
        out.push_str(&format!("idempotent: {}\n", self.idempotent));
        out.push_str(&format!("trust_level: {}\n", self.trust_level));
        out.push_str(&format!("module_count: {}\n", self.module_count));
        if let Some(ref schema) = self.output_schema {
            out.push_str(&format!("output_schema: {}\n", schema));
        }
        if !self.allowed_tools.is_empty() {
            out.push_str(&format!("allowed_tools: {}\n", self.allowed_tools.join(", ")));
        }
        out.push_str("---\n\n");
        out.push_str(&self.body);
        out
    }
}

fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

// ── Telemetry Sidecar ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillTelemetry {
    pub use_count: u64,
    pub view_count: u64,
    pub last_used_at: u64,
    pub patch_count: u64,
    pub state: SkillState,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SkillState {
    Active,
    Stale,
    Archived,
}

impl Default for SkillState {
    fn default() -> Self {
        SkillState::Active
    }
}

/// Gerencia lifecycle automático: active → stale (30 dias) → archived (90 dias).
pub struct SkillTelemetrySidecar {
    stale_days: u64,
    archive_days: u64,
}

impl SkillTelemetrySidecar {
    pub fn new() -> Self {
        Self {
            stale_days: 30,
            archive_days: 90,
        }
    }

    pub fn with_limits(mut self, stale: u64, archive: u64) -> Self {
        self.stale_days = stale;
        self.archive_days = archive;
        self
    }

    pub fn update_on_use(&self, telemetry: &mut SkillTelemetry) {
        telemetry.use_count += 1;
        telemetry.last_used_at = now_epoch_secs();
        if telemetry.state == SkillState::Archived && !telemetry.pinned {
            telemetry.state = SkillState::Active;
        }
    }

    pub fn update_on_view(&self, telemetry: &mut SkillTelemetry) {
        telemetry.view_count += 1;
    }

    pub fn evaluate_state(&self, telemetry: &mut SkillTelemetry) {
        if telemetry.pinned {
            return; // pinned skills são imunes
        }
        let now = now_epoch_secs();
        let days_since_use = (now - telemetry.last_used_at) / 86400;
        if days_since_use >= self.archive_days {
            telemetry.state = SkillState::Archived;
        } else if days_since_use >= self.stale_days {
            telemetry.state = SkillState::Stale;
        }
    }
}

impl Default for SkillTelemetrySidecar {
    fn default() -> Self {
        Self::new()
    }
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SKILL_MD: &str = r#"---
name: rust-api
description: Build REST APIs in Rust
version: 1.0.0
license: MIT
platforms: linux, macos, windows
prerequisites: cargo, rustc
tags: backend, api, rust
related_skills: rust-auth, rust-db
---

## When to Use
Use this skill when building HTTP APIs with Axum or Actix.

## When NOT to Use
Do NOT use for GraphQL — use rust-graphql instead.

## Quick Reference
- `cargo new my-api`
- Add `axum` to Cargo.toml

## Examples
```rust
async fn handler() -> &'static str { "Hello" }
```
"#;

    #[test]
    fn parse_skill_md_frontmatter() {
        let skill = SkillMd::parse(SAMPLE_SKILL_MD).unwrap();
        assert_eq!(skill.name, "rust-api");
        assert_eq!(skill.version, "1.0.0");
        assert_eq!(skill.license, Some("MIT".to_string()));
        assert_eq!(skill.platforms, vec!["linux", "macos", "windows"]);
        assert_eq!(skill.prerequisites, vec!["cargo", "rustc"]);
        assert_eq!(skill.tags, vec!["backend", "api", "rust"]);
        assert_eq!(skill.related_skills, vec!["rust-auth", "rust-db"]);
    }

    #[test]
    fn parse_skill_md_sections() {
        let skill = SkillMd::parse(SAMPLE_SKILL_MD).unwrap();
        assert!(skill.when_to_use.contains("Axum"));
        assert!(skill.when_not_to_use.contains("GraphQL"));
        assert!(skill.quick_reference.contains("cargo new"));
        assert!(skill.examples.contains("handler"));
    }

    #[test]
    fn parse_rejects_missing_frontmatter() {
        let result = SkillMd::parse("no frontmatter here");
        assert!(result.is_err());
    }

    #[test]
    fn parse_rejects_missing_name() {
        let result = SkillMd::parse("---\nversion: 1.0\n---\nbody");
        assert!(result.is_err());
    }

    #[test]
    fn roundtrip_markdown() {
        let skill = SkillMd::parse(SAMPLE_SKILL_MD).unwrap();
        let md = skill.to_markdown();
        let reparsed = SkillMd::parse(&md).unwrap();
        assert_eq!(skill.name, reparsed.name);
        assert_eq!(skill.version, reparsed.version);
    }

    #[test]
    fn telemetry_lifecycle() {
        let sidecar = SkillTelemetrySidecar::new().with_limits(1, 2);
        let mut tel = SkillTelemetry {
            use_count: 0,
            view_count: 0,
            last_used_at: now_epoch_secs(),
            patch_count: 0,
            state: SkillState::Active,
            pinned: false,
        };

        sidecar.update_on_use(&mut tel);
        assert_eq!(tel.use_count, 1);

        // Simula stale
        tel.last_used_at = now_epoch_secs() - 86401;
        sidecar.evaluate_state(&mut tel);
        assert_eq!(tel.state, SkillState::Stale);

        // Simula archive
        tel.last_used_at = now_epoch_secs() - 172801;
        sidecar.evaluate_state(&mut tel);
        assert_eq!(tel.state, SkillState::Archived);
    }

    #[test]
    fn pinned_skill_immune() {
        let sidecar = SkillTelemetrySidecar::new().with_limits(1, 2);
        let mut tel = SkillTelemetry {
            use_count: 0,
            view_count: 0,
            last_used_at: now_epoch_secs() - 172801,
            patch_count: 0,
            state: SkillState::Active,
            pinned: true,
        };

        sidecar.evaluate_state(&mut tel);
        assert_eq!(tel.state, SkillState::Active);
    }

    #[test]
    fn telemetry_use_resets_archive() {
        let sidecar = SkillTelemetrySidecar::new();
        let mut tel = SkillTelemetry {
            use_count: 0,
            view_count: 0,
            last_used_at: 0,
            patch_count: 0,
            state: SkillState::Archived,
            pinned: false,
        };

        sidecar.update_on_use(&mut tel);
        assert_eq!(tel.state, SkillState::Active);
    }
}
