use anyhow::{bail, Result};
use regex::Regex;

/// Verificação de segurança de paths de arquivo.
pub struct FileSafetyChecker {
    sensitive_patterns: Vec<Regex>,
    safe_write_root: Option<String>,
    blocked_prefixes: Vec<String>,
}

impl FileSafetyChecker {
    pub fn new(safe_write_root: Option<&str>) -> Self {
        let patterns = vec![
            Regex::new(
                r"(?i)(\.ssh[/\\]|\.aws[/\\]|\.gnupg[/\\]|/etc/sudoers|/etc/shadow|/etc/passwd)",
            )
            .unwrap(),
            Regex::new(r"(?i)(\.env$|\.env\.|secrets?\.|credentials?\.)").unwrap(),
        ];
        let prefixes = vec![
            "~/.ssh".into(),
            "~/.aws".into(),
            "~/.gnupg".into(),
            "/etc".into(),
            "/usr".into(),
            "/bin".into(),
            "/sbin".into(),
            "C:\\Windows".into(),
            "C:\\Program Files".into(),
        ];
        Self {
            sensitive_patterns: patterns,
            safe_write_root: safe_write_root.map(String::from),
            blocked_prefixes: prefixes,
        }
    }

    pub fn check_write(&self, path: &str) -> Result<()> {
        let normalized = path.replace("\\", "/");

        // Verifica safe-write-root
        if let Some(ref root) = self.safe_write_root {
            let root_norm = root.replace("\\", "/");
            if !normalized.starts_with(&root_norm) {
                bail!(
                    "FileSafety: escrita fora do safe-write-root ({}): {}",
                    root,
                    path
                );
            }
        }

        // Verifica prefixos bloqueados
        for prefix in &self.blocked_prefixes {
            if normalized.starts_with(&prefix.replace("\\", "/")) {
                bail!("FileSafety: escrita em path sensível bloqueado: {}", path);
            }
        }

        // Verifica padrões regex
        for re in &self.sensitive_patterns {
            if re.is_match(&normalized) {
                bail!("FileSafety: padrão sensível detectado em: {}", path);
            }
        }

        Ok(())
    }

    pub fn check_read(&self, path: &str) -> Result<()> {
        let normalized = path.replace("\\", "/");
        // Bloqueia leitura de caches internos do agente para prevenir prompt injection
        if normalized.contains("/.arreio/") && normalized.contains("blackboard") {
            // Permite — é parte do sistema
            return Ok(());
        }
        // Bloqueia leitura de chaves privadas
        if normalized.contains("/.ssh/id_") || normalized.contains("/.aws/credentials") {
            bail!("FileSafety: leitura de credenciais bloqueada: {}", path);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloqueia_escrita_ssh() {
        let checker = FileSafetyChecker::new(Some("/workspace"));
        assert!(checker.check_write("~/.ssh/id_rsa").is_err());
    }

    #[test]
    fn bloqueia_fora_safe_root() {
        let checker = FileSafetyChecker::new(Some("/workspace"));
        assert!(checker.check_write("/tmp/out.rs").is_err());
    }

    #[test]
    fn permite_dentro_safe_root() {
        let checker = FileSafetyChecker::new(Some("/workspace"));
        assert!(checker.check_write("/workspace/src/main.rs").is_ok());
    }
}
