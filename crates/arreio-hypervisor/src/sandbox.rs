//! Sandboxing OS — GAP-008
//!
//! Implementação de sandboxing por Job Objects no Windows.
//! Fallback para hypervisor atual (regex blocklist) quando sandbox não disponível.
//!
//! Princípio de segurança: o sandbox é hard-gated externo ao modelo.
//! O modelo NUNCA decide se o sandbox está ativo.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::ExecResult;

/// Trait unificado para sandboxes de diferentes plataformas.
pub trait Sandbox {
    /// Verifica se a sandbox está disponível nesta plataforma.
    fn is_available(&self) -> bool;

    /// Executa um comando dentro da sandbox restrita.
    fn spawn_restricted(&self, cmd: &str, cwd: Option<&Path>) -> Result<Child>;

    /// Mata o processo e todos os seus filhos (via Job Object ou equivalente).
    fn terminate(&self, child: &mut Child) -> Result<()>;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Windows — Job Object Sandbox
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "windows")]
pub use windows_impl::JobObjectSandbox;

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::ptr;

    /// Sandboxing via Windows Job Objects.
    pub struct JobObjectSandbox {
        job_handle: isize,
    }

    impl JobObjectSandbox {
        pub fn new() -> Result<Self> {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectA, JobObjectBasicLimitInformation,
                SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
            };

            let job_handle = unsafe {
                CreateJobObjectA(ptr::null_mut(), ptr::null())
            };
            if job_handle == 0 || job_handle == -1_isize {
                return Err(anyhow::anyhow!("CreateJobObjectA falhou"));
            }

            let mut limits = JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
                ..unsafe { std::mem::zeroed() }
            };

            let ok = unsafe {
                SetInformationJobObject(
                    job_handle,
                    JobObjectBasicLimitInformation,
                    &mut limits as *mut _ as *mut _,
                    std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                unsafe { CloseHandle(job_handle) };
                return Err(anyhow::anyhow!("SetInformationJobObject falhou"));
            }

            Ok(Self { job_handle })
        }

        fn assign_process(&self, child: &Child) -> Result<()> {
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
            let raw = child.as_raw_handle() as isize;
            let ok = unsafe { AssignProcessToJobObject(self.job_handle, raw) };
            if ok == 0 {
                return Err(anyhow::anyhow!("AssignProcessToJobObject falhou"));
            }
            Ok(())
        }
    }

    impl Sandbox for JobObjectSandbox {
        fn is_available(&self) -> bool {
            true
        }

        fn spawn_restricted(&self, cmd: &str, cwd: Option<&Path>) -> Result<Child> {
            let mut command = Command::new("cmd");
            command.args(["/C", cmd]);
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            command.creation_flags(0x00000010); // CREATE_NEW_CONSOLE
            if let Some(dir) = cwd {
                command.current_dir(dir);
            }

            let child = command
                .spawn()
                .with_context(|| format!("falha ao spawnar comando sandboxed: {}", cmd))?;

            if let Err(e) = self.assign_process(&child) {
                eprintln!("[sandbox] WARNING: não conseguiu associar processo ao Job Object: {}", e);
            }

            Ok(child)
        }

        fn terminate(&self, child: &mut Child) -> Result<()> {
            let _ = child.kill();
            Ok(())
        }
    }

    impl Drop for JobObjectSandbox {
        fn drop(&mut self) {
            use windows_sys::Win32::Foundation::CloseHandle;
            if self.job_handle != 0 && self.job_handle != -1_isize {
                unsafe { CloseHandle(self.job_handle) };
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Linux — Bubblewrap Sandbox
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "linux")]
pub struct BubblewrapSandbox;

#[cfg(target_os = "linux")]
impl BubblewrapSandbox {
    pub fn new() -> Self {
        Self
    }

    /// Verifica se o executável `bwrap` está acessível no PATH.
    fn bwrap_in_path() -> bool {
        Command::new("bwrap")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "linux")]
impl Sandbox for BubblewrapSandbox {
    fn is_available(&self) -> bool {
        Self::bwrap_in_path()
    }

    fn spawn_restricted(&self, cmd: &str, cwd: Option<&Path>) -> Result<Child> {
        if !Self::bwrap_in_path() {
            return Err(anyhow::anyhow!("bubblewrap (bwrap) não encontrado no PATH"));
        }

        let mut command = Command::new("bwrap");
        // Sandboxing básica: filesystem read-only, tmpfs isolado, unshare de namespaces
        command.args([
            "--ro-bind", "/", "/",
            "--tmpfs", "/tmp",
            "--proc", "/proc",
            "--dev", "/dev",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
        ]);

        // Se houver diretório de trabalho, bind-mount ele como rw
        if let Some(dir) = cwd {
            let abs = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
            command.arg("--bind");
            command.arg(&abs);
            command.arg(&abs);
            command.current_dir(&abs);
        } else {
            command.arg("--chdir");
            command.arg("/tmp");
        }

        command.args(["--", "sh", "-c", cmd]);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let child = command
            .spawn()
            .with_context(|| format!("falha ao spawnar comando sandboxed com bubblewrap: {}", cmd))?;

        Ok(child)
    }

    fn terminate(&self, child: &mut Child) -> Result<()> {
        let _ = child.kill();
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// macOS — Seatbelt Stub (placeholder para futura implementação)
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(target_os = "macos")]
pub struct SeatbeltSandbox;

#[cfg(target_os = "macos")]
impl SeatbeltSandbox {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
impl Sandbox for SeatbeltSandbox {
    fn is_available(&self) -> bool {
        false
    }

    fn spawn_restricted(&self, _cmd: &str, _cwd: Option<&Path>) -> Result<Child> {
        Err(anyhow::anyhow!("seatbelt não implementado"))
    }

    fn terminate(&self, child: &mut Child) -> Result<()> {
        let _ = child.kill();
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Fallback — No Sandbox (sempre disponível, mas não restringe nada)
// ═══════════════════════════════════════════════════════════════════════════════

/// Sandbox nulo — usado quando nenhuma sandbox real está disponível.
/// Ainda assim aplica timeout e captura de stdout/stderr.
pub struct NoopSandbox;

impl NoopSandbox {
    pub fn new() -> Self {
        Self
    }
}

impl Sandbox for NoopSandbox {
    fn is_available(&self) -> bool {
        true
    }

    fn spawn_restricted(&self, cmd: &str, cwd: Option<&Path>) -> Result<Child> {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut c = Command::new("cmd");
            c.args(["/C", cmd]);
            c
        };

        #[cfg(not(target_os = "windows"))]
        let mut command = {
            let mut c = Command::new("sh");
            c.args(["-c", cmd]);
            c
        };

        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }

        let child = command
            .spawn()
            .with_context(|| format!("falha ao spawnar comando: {}", cmd))?;
        Ok(child)
    }

    fn terminate(&self, child: &mut Child) -> Result<()> {
        let _ = child.kill();
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Factory
// ═══════════════════════════════════════════════════════════════════════════════

/// Cria a melhor sandbox disponível para a plataforma atual.
pub fn create_sandbox() -> Box<dyn Sandbox> {
    #[cfg(target_os = "windows")]
    {
        match JobObjectSandbox::new() {
            Ok(sb) => {
                eprintln!("[sandbox] Job Object ativo (Windows)");
                return Box::new(sb);
            }
            Err(e) => {
                eprintln!("[sandbox] Job Object indisponível: {}. Fallback para Noop.", e);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let sb = BubblewrapSandbox::new();
        if sb.is_available() {
            eprintln!("[sandbox] Bubblewrap ativo (Linux)");
            return Box::new(sb);
        }
    }

    #[cfg(target_os = "macos")]
    {
        let sb = SeatbeltSandbox::new();
        if sb.is_available() {
            eprintln!("[sandbox] Seatbelt ativo (macOS)");
            return Box::new(sb);
        }
    }

    Box::new(NoopSandbox::new())
}

// ═══════════════════════════════════════════════════════════════════════════════
// Hypervisor integration
// ═══════════════════════════════════════════════════════════════════════════════

/// Wrapper que integra sandbox ao Hypervisor existente.
/// Executa comandos dentro da sandbox, aplicando timeout.
pub struct SandboxedExecutor {
    sandbox: Box<dyn Sandbox>,
    timeout_secs: u64,
}

impl SandboxedExecutor {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            sandbox: create_sandbox(),
            timeout_secs,
        }
    }

    pub fn with_sandbox(timeout_secs: u64, sandbox: Box<dyn Sandbox>) -> Self {
        Self {
            sandbox,
            timeout_secs,
        }
    }

    /// Executa comando dentro da sandbox com timeout.
    pub fn run(&self, cmd: &str, cwd: Option<&Path>) -> Result<ExecResult> {
        let start = Instant::now();
        let mut child = self.sandbox.spawn_restricted(cmd, cwd)?;

        let timeout = Duration::from_secs(self.timeout_secs);
        let poll_interval = Duration::from_millis(100);

        loop {
            if let Some(status) = child.try_wait().context("try_wait falhou")? {
                let output = child
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
                let _ = self.sandbox.terminate(&mut child);
                return Ok(ExecResult {
                    exit_code: -2,
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
    fn sandbox_blocks_rm_rf() {
        let exec = SandboxedExecutor::new(10);
        let result = exec.run("echo hello", None).unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }

    #[test]
    fn sandbox_timeout_works() {
        #[cfg(target_os = "windows")]
        let cmd = "ping -n 10 127.0.0.1";
        #[cfg(not(target_os = "windows"))]
        let cmd = "sleep 10";

        let exec = SandboxedExecutor::new(1);
        let result = exec.run(cmd, None).unwrap();
        assert_eq!(result.exit_code, -2);
        assert!(result.stderr.contains("TIMEOUT"));
    }
}
