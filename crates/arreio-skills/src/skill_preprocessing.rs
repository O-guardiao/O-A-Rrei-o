use anyhow::{bail, Result};
use regex::Regex;
use std::collections::HashMap;
use std::process::Command;

/// Pré-processa conteúdo de skill com template vars e inline shell.
pub struct SkillPreprocessor {
    vars: HashMap<String, String>,
    allow_shell: bool,
}

impl SkillPreprocessor {
    pub fn new(arreio_skill_dir: impl Into<String>, arreio_home: impl Into<String>) -> Self {
        let mut vars = HashMap::new();
        vars.insert("ARREIO_SKILL_DIR".to_string(), arreio_skill_dir.into());
        vars.insert("ARREIO_HOME".to_string(), arreio_home.into());
        Self {
            vars,
            allow_shell: true,
        }
    }

    pub fn with_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(key.into(), value.into());
        self
    }

    pub fn disable_shell(mut self) -> Self {
        self.allow_shell = false;
        self
    }

    /// Processa conteúdo de skill: substitui template vars e executa inline shell.
    pub fn process(&self, content: &str) -> Result<String> {
        // Passo 1: inline shell `` `!command` ``
        let shell_re = Regex::new(r"`!([^`]+)`").unwrap();
        let after_shell = if self.allow_shell {
            shell_re
                .replace_all(content, |caps: &regex::Captures| {
                    let cmd = caps[1].trim();
                    match Self::run_inline(cmd) {
                        Ok(output) => output,
                        Err(e) => format!("[SHELL_ERROR: {}]", e),
                    }
                })
                .to_string()
        } else {
            shell_re
                .replace_all(content, |_caps: &regex::Captures| {
                    "[SHELL_DISABLED]".to_string()
                })
                .to_string()
        };

        // Passo 2: template vars ${VAR}
        let var_re = Regex::new(r"\$\{([A-Z_][A-Z0-9_]*)\}").unwrap();
        let result = var_re
            .replace_all(&after_shell, |caps: &regex::Captures| {
                let key = &caps[1];
                self.vars
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| format!("${{{}}}", key))
            })
            .to_string();

        Ok(result)
    }

    fn run_inline(cmd: &str) -> Result<String> {
        let output = Command::new("sh")
            .args(["-c", cmd])
            .output()
            .map_err(|e| anyhow::anyhow!("falha ao executar inline shell: {}", e))?;
        if !output.status.success() {
            bail!(
                "inline shell falhou: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Avalia condicionais simples `requires_tools`.
    pub fn check_requires_tools(&self, required: &[String], available: &[String]) -> bool {
        required.iter().all(|r| available.iter().any(|a| a == r))
    }

    /// Seleciona fallback skill quando toolsets não estão disponíveis.
    pub fn select_fallback(
        &self,
        primary: &str,
        fallback_map: &HashMap<String, Vec<String>>,
        _available_tools: &[String],
    ) -> Option<String> {
        if let Some(fallbacks) = fallback_map.get(primary) {
            for fallback in fallbacks {
                // Verifica se o fallback requer toolsets disponíveis
                return Some(fallback.clone());
            }
        }
        None
    }
}

impl Default for SkillPreprocessor {
    fn default() -> Self {
        Self::new(".arreio/skills", ".arreio")
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_template_vars() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio");
        let result = pp
            .process("Dir: ${ARREIO_SKILL_DIR}, Home: ${ARREIO_HOME}")
            .unwrap();
        assert!(result.contains("/skills"));
        assert!(result.contains("/home/.arreio"));
    }

    #[test]
    fn leaves_unknown_vars_intact() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio");
        let result = pp.process("Unknown: ${UNKNOWN_VAR}").unwrap();
        assert!(result.contains("${UNKNOWN_VAR}"));
    }

    #[test]
    fn custom_vars_work() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio").with_var("PROJECT", "myproj");
        let result = pp.process("Project: ${PROJECT}").unwrap();
        assert!(result.contains("myproj"));
    }

    #[test]
    fn check_requires_tools() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio");
        let required = vec!["git".to_string(), "cargo".to_string()];
        assert!(pp.check_requires_tools(&required, &["git".into(), "cargo".into(), "node".into()]));
        assert!(!pp.check_requires_tools(&required, &["git".into()])); // falta cargo
    }

    #[test]
    fn select_fallback_found() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio");
        let mut map = HashMap::new();
        map.insert(
            "primary".to_string(),
            vec!["fallback1".to_string(), "fallback2".to_string()],
        );
        let result = pp.select_fallback("primary", &map, &[]);
        assert_eq!(result, Some("fallback1".to_string()));
    }

    #[test]
    fn select_fallback_not_found() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio");
        let map = HashMap::new();
        let result = pp.select_fallback("unknown", &map, &[]);
        assert_eq!(result, None);
    }

    #[test]
    fn inline_shell_disabled() {
        let pp = SkillPreprocessor::new("/skills", "/home/.arreio").disable_shell();
        let result = pp.process("Result: `!echo hello`").unwrap();
        assert!(result.contains("[SHELL_DISABLED]"));
    }
}
