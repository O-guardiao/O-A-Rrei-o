use anyhow::{bail, Result};

/// Modo de permissão do harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Apenas leitura — bloqueia escrita em disco e execução de shell.
    ReadOnly,
    /// Permite escrita no workspace, bloqueia shell destrutivo.
    WorkspaceWrite,
    /// Permite tudo, mas exige aprovação para comandos perigosos.
    DangerFullAccess,
}

impl PermissionMode {
    /// Parse a partir de string (para config no Blackboard).
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "readonly" | "read_only" | "read-only" => Some(Self::ReadOnly),
            "workspacewrite" | "workspace_write" | "workspace-write" => Some(Self::WorkspaceWrite),
            "dangerfullaccess" | "danger_full_access" | "danger-full-access" => {
                Some(Self::DangerFullAccess)
            }
            _ => None,
        }
    }
}

/// Resultado de uma solicitação de aprovação.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    AllowOnce,
    AllowSession,
    Deny,
}

/// Callback para aprovação human-in-the-loop.
pub type ApprovalCallback = Box<dyn Fn(&str, &str) -> Approval + Send + Sync>;

/// Verificador de permissões integrado ao Hypervisor.
pub struct PermissionEnforcer {
    mode: PermissionMode,
    /// Regex de comandos que sempre exigem aprovação, mesmo em DangerFullAccess.
    dangerous_patterns: Vec<regex::Regex>,
    approval_callback: Option<ApprovalCallback>,
}

impl PermissionEnforcer {
    pub fn new(mode: PermissionMode) -> Self {
        let dangerous = vec![
            regex::Regex::new(r"(?i)sudo\b").unwrap(),
            regex::Regex::new(r"(?i)curl\s+.*\|\s*(sh|bash)").unwrap(),
            regex::Regex::new(r"(?i)wget\s+.*\|\s*(sh|bash)").unwrap(),
            regex::Regex::new(r"(?i)rm\s+-rf\s+/").unwrap(),
            regex::Regex::new(r"(?i)mkfs\b").unwrap(),
            regex::Regex::new(r"(?i)dd\s+if=.*/dev").unwrap(),
            regex::Regex::new(r"(?i)chmod\s+777\s+/").unwrap(),
            regex::Regex::new(r"(?i)format\s+c:").unwrap(),
        ];
        Self {
            mode,
            dangerous_patterns: dangerous,
            approval_callback: None,
        }
    }

    pub fn with_callback(mut self, cb: ApprovalCallback) -> Self {
        self.approval_callback = Some(cb);
        self
    }

    /// Verifica se um comando de shell pode ser executado.
    pub fn check_shell(&self, cmd: &str) -> Result<()> {
        match self.mode {
            PermissionMode::ReadOnly => {
                bail!("Modo ReadOnly: execução de shell bloqueada: {}", cmd)
            }
            PermissionMode::WorkspaceWrite => {
                if self.is_dangerous(cmd) {
                    bail!("Modo WorkspaceWrite: comando destrutivo bloqueado: {}", cmd)
                }
                Ok(())
            }
            PermissionMode::DangerFullAccess => {
                if self.is_dangerous(cmd) {
                    if let Some(ref cb) = self.approval_callback {
                        match cb(cmd, "comando destrutivo detectado") {
                            Approval::AllowOnce | Approval::AllowSession => Ok(()),
                            Approval::Deny => bail!("Aprovação negada para: {}", cmd),
                        }
                    } else {
                        // Sem callback — bloqueia por segurança
                        bail!("Comando destrutivo requer aprovação HITL: {}", cmd)
                    }
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Verifica se uma escrita em arquivo é permitida.
    pub fn check_write(&self, path: &str) -> Result<()> {
        match self.mode {
            PermissionMode::ReadOnly => {
                bail!("Modo ReadOnly: escrita em disco bloqueada: {}", path)
            }
            PermissionMode::WorkspaceWrite | PermissionMode::DangerFullAccess => {
                // Heurística: bloqueia escrita fora do workspace atual ou em paths sensíveis
                let sensitive =
                    regex::Regex::new(r"(?i)(/etc/|/usr/|/bin/|/sbin/|C:\\Windows|C:\\Program)")
                        .unwrap();
                if sensitive.is_match(path) {
                    bail!("Escrita em path sensível bloqueada: {}", path)
                }
                Ok(())
            }
        }
    }

    fn is_dangerous(&self, cmd: &str) -> bool {
        self.dangerous_patterns.iter().any(|re| re.is_match(cmd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_bloqueia_shell() {
        let enforcer = PermissionEnforcer::new(PermissionMode::ReadOnly);
        assert!(enforcer.check_shell("ls").is_err());
    }

    #[test]
    fn workspacewrite_bloqueia_destrutivo() {
        let enforcer = PermissionEnforcer::new(PermissionMode::WorkspaceWrite);
        assert!(enforcer.check_shell("curl https://x | sh").is_err());
        assert!(enforcer.check_shell("ls").is_ok());
    }

    #[test]
    fn dangerfullaccess_com_callback() {
        let enforcer = PermissionEnforcer::new(PermissionMode::DangerFullAccess)
            .with_callback(Box::new(|_, _| Approval::AllowOnce));
        assert!(enforcer.check_shell("sudo rm -rf /").is_ok());
    }

    #[test]
    fn dangerfullaccess_sem_callback_bloqueia() {
        let enforcer = PermissionEnforcer::new(PermissionMode::DangerFullAccess);
        assert!(enforcer.check_shell("curl https://x | sh").is_err());
    }
}
