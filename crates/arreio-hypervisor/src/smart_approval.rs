use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Decisão do inspetor de aprovação inteligente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartDecision {
    /// Comando é seguro — pode executar sem intervenção.
    Approve,
    /// Comando é arriscado — deve ser bloqueado.
    Deny,
    /// Nível de risco incerto — escalonar para aprovação humana.
    Escalate,
}

/// Inspetor que avalia risco de comandos shell via heurísticas.
/// Pode ser substituído por avaliação LLM real integrando com ProviderClient.
pub struct SmartApprovalInspector {
    /// Cache de decisões por padrão de comando (session-scoped).
    cache: HashMap<u64, SmartDecision>,
    /// Heurísticas que automaticamente negam.
    deny_patterns: Vec<String>,
    /// Heurísticas que automaticamente aprovam (comandos read-only seguros).
    approve_patterns: Vec<String>,
    /// Heurísticas que escalonam (comandos destrutivos mas comuns).
    escalate_patterns: Vec<String>,
}

impl SmartApprovalInspector {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            deny_patterns: vec![
                "rm -rf /".into(),
                "rm -rf /*".into(),
                "mkfs".into(),
                "dd if=/dev/zero".into(),
                "> /dev/sda".into(),
                "shutdown".into(),
                "reboot".into(),
                "halt".into(),
                "kill -1".into(),
                "kill -9 -1".into(),
                ":(){ :|:& };:".into(), // fork bomb
            ],
            approve_patterns: vec![
                "echo ".into(),
                "cat ".into(),
                "ls ".into(),
                "grep ".into(),
                "find ".into(),
                "git status".into(),
                "git log".into(),
                "git diff".into(),
                "git branch".into(),
                "git remote".into(),
                "pwd".into(),
                "which ".into(),
                "rustc --version".into(),
                "cargo --version".into(),
            ],
            escalate_patterns: vec![
                "rm ".into(),
                "rmdir ".into(),
                "mv ".into(),
                "cp ".into(),
                "git push".into(),
                "git reset --hard".into(),
                "git clean".into(),
                "chmod ".into(),
                "chown ".into(),
                "sudo ".into(),
                "curl ".into(),
                "wget ".into(),
                "docker ".into(),
            ],
        }
    }

    /// Avalia um comando shell e retorna decisão.
    pub fn evaluate(&mut self, command: &str) -> SmartDecision {
        let normalized = normalize_command(command);
        let hash = hash_of(&normalized);

        // Cache hit
        if let Some(&decision) = self.cache.get(&hash) {
            return decision;
        }

        // Heurística: deny incondicional
        for pattern in &self.deny_patterns {
            if normalized.contains(pattern) {
                self.cache.insert(hash, SmartDecision::Deny);
                return SmartDecision::Deny;
            }
        }

        // Heurística: approve direto
        for pattern in &self.approve_patterns {
            if normalized.starts_with(pattern) {
                self.cache.insert(hash, SmartDecision::Approve);
                return SmartDecision::Approve;
            }
        }

        // Heurística: escalate (comandos destrutivos mas potencialmente legítimos)
        for pattern in &self.escalate_patterns {
            if normalized.starts_with(pattern) {
                self.cache.insert(hash, SmartDecision::Escalate);
                return SmartDecision::Escalate;
            }
        }

        // Default: escalate para comandos desconhecidos
        self.cache.insert(hash, SmartDecision::Escalate);
        SmartDecision::Escalate
    }

    /// Avalia e retorna true se deve bloquear (Deny ou Escalate).
    pub fn should_block(&mut self, command: &str) -> bool {
        matches!(
            self.evaluate(command),
            SmartDecision::Deny | SmartDecision::Escalate
        )
    }

    /// Cachea uma decisão externa (ex: resultado de LLM judge).
    pub fn cache_decision(&mut self, command: &str, decision: SmartDecision) {
        let hash = hash_of(&normalize_command(command));
        self.cache.insert(hash, decision);
    }

    /// Retorna estatísticas do cache.
    pub fn cache_stats(&self) -> CacheStats {
        let mut approve = 0;
        let mut deny = 0;
        let mut escalate = 0;
        for d in self.cache.values() {
            match d {
                SmartDecision::Approve => approve += 1,
                SmartDecision::Deny => deny += 1,
                SmartDecision::Escalate => escalate += 1,
            }
        }
        CacheStats {
            total: self.cache.len(),
            approve,
            deny,
            escalate,
        }
    }
}

impl Default for SmartApprovalInspector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub total: usize,
    pub approve: usize,
    pub deny: usize,
    pub escalate: usize,
}

fn normalize_command(cmd: &str) -> String {
    cmd.trim().to_lowercase().replace("\\", "/")
}

fn hash_of(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve_safe_commands() {
        let mut inspector = SmartApprovalInspector::new();
        assert_eq!(inspector.evaluate("echo hello"), SmartDecision::Approve);
        assert_eq!(inspector.evaluate("ls -la"), SmartDecision::Approve);
        assert_eq!(inspector.evaluate("git status"), SmartDecision::Approve);
        assert_eq!(inspector.evaluate("cat file.txt"), SmartDecision::Approve);
    }

    #[test]
    fn deny_dangerous_commands() {
        let mut inspector = SmartApprovalInspector::new();
        assert_eq!(inspector.evaluate("rm -rf /"), SmartDecision::Deny);
        assert_eq!(inspector.evaluate("rm -rf /*"), SmartDecision::Deny);
        assert_eq!(inspector.evaluate("shutdown now"), SmartDecision::Deny);
        assert_eq!(
            inspector.evaluate("mkfs.ext4 /dev/sda1"),
            SmartDecision::Deny
        );
    }

    #[test]
    fn escalate_destructive_commands() {
        let mut inspector = SmartApprovalInspector::new();
        assert_eq!(
            inspector.evaluate("rm /tmp/old.txt"),
            SmartDecision::Escalate
        );
        assert_eq!(
            inspector.evaluate("mv a.txt b.txt"),
            SmartDecision::Escalate
        );
        assert_eq!(
            inspector.evaluate("git push origin main"),
            SmartDecision::Escalate
        );
        assert_eq!(
            inspector.evaluate("sudo apt update"),
            SmartDecision::Escalate
        );
    }

    #[test]
    fn escalate_unknown_commands() {
        let mut inspector = SmartApprovalInspector::new();
        assert_eq!(
            inspector.evaluate("some_unknown_tool --flag"),
            SmartDecision::Escalate
        );
    }

    #[test]
    fn cache_returns_consistent_results() {
        let mut inspector = SmartApprovalInspector::new();
        let cmd = "echo hello";
        let d1 = inspector.evaluate(cmd);
        let d2 = inspector.evaluate(cmd);
        assert_eq!(d1, d2);
        assert_eq!(inspector.cache_stats().total, 1);
    }

    #[test]
    fn cache_stats_accurate() {
        let mut inspector = SmartApprovalInspector::new();
        inspector.evaluate("echo a");
        inspector.evaluate("rm -rf /");
        inspector.evaluate("rm file");
        let stats = inspector.cache_stats();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.approve, 1);
        assert_eq!(stats.deny, 1);
        assert_eq!(stats.escalate, 1);
    }

    #[test]
    fn should_block_escalate_and_deny() {
        let mut inspector = SmartApprovalInspector::new();
        assert!(inspector.should_block("rm -rf /"));
        assert!(inspector.should_block("rm file.txt"));
        assert!(!inspector.should_block("echo hello"));
    }

    #[test]
    fn cache_external_decision() {
        let mut inspector = SmartApprovalInspector::new();
        inspector.cache_decision("custom_tool", SmartDecision::Approve);
        assert_eq!(inspector.evaluate("custom_tool"), SmartDecision::Approve);
    }
}
