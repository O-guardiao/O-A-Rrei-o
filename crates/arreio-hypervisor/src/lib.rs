pub mod bash_analyzer;
pub mod bash_security;
pub mod file_safety;
pub mod interceptor;
pub mod permissions;
pub mod sandbox;
pub mod smart_approval;
pub mod tool_guardrails;
pub mod watchdog;

use anyhow::{Context, Result};
use interceptor::Interceptor;
use permissions::PermissionEnforcer;
use std::process::Command;
use std::time::{Duration, Instant};

pub use bash_analyzer::{BashAnalysis, BashAnalyzer, CommandType};
pub use bash_security::{BashCheckId, BashSecurityChecker, BashSecurityViolation};
pub use interceptor::BlockedError;
pub use permissions::{Approval, ApprovalCallback};
pub use sandbox::{Sandbox, SandboxedExecutor, create_sandbox, NoopSandbox};
pub use smart_approval::{CacheStats, SmartApprovalInspector, SmartDecision};
pub use watchdog::Watchdog;

// ── Resultado de execução ─────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
    /// Se true, indica que o comando foi bloqueado por política de segurança
    /// antes de ser executado. O stderr contém o motivo.
    pub permission_denied: bool,
}

// ── Hypervisor ────────────────────────────────────────────────────────────────

/// O Escudo: toda execução de código gerado pela IA passa por aqui.
/// 1. Interceptor bloqueia comandos destrutivos
/// 2. BashAnalyzer valida semântica
/// 3. PermissionEnforcer aplica modos de permissão
/// 4. Executa com timeout
/// 5. Retorna resultado para o Watchdog avaliar
pub struct Hypervisor {
    security_checker: BashSecurityChecker,
    interceptor: Interceptor,
    analyzer: BashAnalyzer,
    enforcer: Option<PermissionEnforcer>,
    timeout_secs: u64,
    use_sandbox: bool,
}

impl Hypervisor {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            security_checker: BashSecurityChecker::new(),
            interceptor: Interceptor::new(),
            analyzer: BashAnalyzer::new(),
            enforcer: None,
            timeout_secs,
            use_sandbox: false,
        }
    }

    /// Configura o PermissionEnforcer (opcional).
    pub fn with_enforcer(mut self, enforcer: PermissionEnforcer) -> Self {
        self.enforcer = Some(enforcer);
        self
    }

    /// Ativa sandboxing OS (Job Objects no Windows, bubblewrap no Linux).
    pub fn with_sandbox(mut self) -> Self {
        self.use_sandbox = true;
        self
    }

    /// Retorna o timeout configurado em segundos.
    pub fn timeout(&self) -> u64 {
        self.timeout_secs
    }

    /// Executa um comando de shell após validação.
    /// No Windows usa `cmd /C`, no Unix usa `sh -c`.
    ///
    /// Quando um comando é bloqueado pelo interceptor ou permission enforcer,
    /// retorna Ok(ExecResult) com exit_code -3 e permission_denied=true,
    /// permitindo que o resultado flua de volta ao modelo como tool_result
    /// (error withholding) em vez de abortar o loop.
    pub fn run(&self, cmd: &str, work_dir: Option<&std::path::Path>) -> Result<ExecResult> {
        // 0. Bash Security 23 Checks (GAP-007) — ANTES do interceptor
        if let Err(violation) = self.security_checker.check_all(cmd) {
            return Ok(ExecResult {
                exit_code: -3,
                stdout: String::new(),
                stderr: format!(
                    "SECURITY CHECK #{}: {}",
                    violation.check_id.as_str(),
                    violation.description
                ),
                elapsed: Duration::from_secs(0),
                permission_denied: true,
            });
        }

        // 1. Interceptor regex blocklist
        if let Err(e) = self.interceptor.check(cmd) {
            return Ok(ExecResult {
                exit_code: -3,
                stdout: String::new(),
                stderr: format!("PERMISSION DENIED: {}", e.reason),
                elapsed: Duration::from_secs(0),
                permission_denied: true,
            });
        }

        // 2. Bash semantic analysis
        let analysis = self.analyzer.analyze(cmd);
        if analysis.is_destructive {
            if let Some(ref reason) = analysis.destructive_reason {
                eprintln!(
                    "[hypervisor] WARNING: comando destrutivo detectado: {}",
                    reason
                );
            }
        }

        // 3. PermissionEnforcer
        if let Some(ref enforcer) = self.enforcer {
            if let Err(e) = enforcer.check_shell(cmd) {
                return Ok(ExecResult {
                    exit_code: -3,
                    stdout: String::new(),
                    stderr: format!("PERMISSION DENIED: {}", e),
                    elapsed: Duration::from_secs(0),
                    permission_denied: true,
                });
            }
            // Workspace boundary check para paths referenciados
            for path in &analysis.referenced_paths {
                if let Err(e) = enforcer.check_write(path) {
                    return Ok(ExecResult {
                        exit_code: -3,
                        stdout: String::new(),
                        stderr: format!("PERMISSION DENIED: {}", e),
                        elapsed: Duration::from_secs(0),
                        permission_denied: true,
                    });
                }
            }
        }

        let start = Instant::now();

        // 4. Execução com sandbox (GAP-008) se ativado
        if self.use_sandbox {
            let executor = SandboxedExecutor::new(self.timeout_secs);
            return executor.run(cmd, work_dir);
        }

        // Fallback: execução direta (sem sandbox)
        #[cfg(target_os = "windows")]
        let mut child = {
            let mut c = Command::new("cmd");
            c.args(["/C", cmd]);
            c
        };

        #[cfg(not(target_os = "windows"))]
        let mut child = {
            let mut c = Command::new("sh");
            c.args(["-c", cmd]);
            c
        };

        if let Some(dir) = work_dir {
            child.current_dir(dir);
        }

        // Captura stdout e stderr
        child.stdout(std::process::Stdio::piped());
        child.stderr(std::process::Stdio::piped());

        let mut handle = child
            .spawn()
            .with_context(|| format!("falha ao iniciar: {}", cmd))?;

        // Implementação de timeout via poll
        let timeout = Duration::from_secs(self.timeout_secs);
        let poll_interval = Duration::from_millis(100);
        loop {
            if let Some(status) = handle.try_wait().context("try_wait falhou")? {
                let output = handle
                    .wait_with_output()
                    .context("wait_with_output falhou")?;
                let elapsed = start.elapsed();
                return Ok(ExecResult {
                    exit_code: status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    elapsed,
                    permission_denied: false,
                });
            }
            if start.elapsed() >= timeout {
                let _ = handle.kill();
                return Ok(ExecResult {
                    exit_code: -2, // código especial: timeout
                    stdout: String::new(),
                    stderr: format!("TIMEOUT após {}s", self.timeout_secs),
                    elapsed: start.elapsed(),
                    permission_denied: false,
                });
            }
            std::thread::sleep(poll_interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_command_returns_permission_denied() {
        let h = Hypervisor::new(10);
        let result = h.run("rm -rf /tmp/test", None).unwrap();
        // Error withholding: bloqueios retornam como Ok(ExecResult) para
        // que o modelo receba o tool_result em vez de abortar o loop.
        assert_eq!(result.exit_code, -3);
        assert!(result.permission_denied);
        assert!(result.stderr.contains("PERMISSION DENIED"));
    }

    #[test]
    fn safe_command_runs_successfully() {
        let h = Hypervisor::new(10);
        #[cfg(target_os = "windows")]
        let result = h.run("echo hello", None).unwrap();
        #[cfg(not(target_os = "windows"))]
        let result = h.run("echo hello", None).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }

    #[test]
    fn timeout_kills_long_running_command() {
        let h = Hypervisor::new(1);
        #[cfg(target_os = "windows")]
        let result = h.run("ping -n 10 127.0.0.1", None).unwrap();
        #[cfg(not(target_os = "windows"))]
        let result = h.run("sleep 10", None).unwrap();
        assert_eq!(result.exit_code, -2);
        assert!(result.stderr.contains("TIMEOUT"));
    }
}
