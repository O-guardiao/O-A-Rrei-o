/// Probing de ambiente de execução.
pub struct EnvironmentProbe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInfo {
    pub is_docker: bool,
    pub is_wsl: bool,
    pub is_ssh: bool,
    pub is_modal: bool,
    pub platform: PlatformHint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformHint {
    Windows,
    Linux,
    MacOS,
    WSL,
    Docker,
    Unknown,
}

impl PlatformHint {
    /// Retorna todas as plataformas suportadas (garante que todas variants são construídas).
    pub fn all() -> Vec<Self> {
        vec![
            Self::Windows,
            Self::Linux,
            Self::MacOS,
            Self::WSL,
            Self::Docker,
            Self::Unknown,
        ]
    }
}

impl EnvironmentProbe {
    pub fn detect() -> EnvironmentInfo {
        EnvironmentInfo {
            is_docker: Self::detect_docker(),
            is_wsl: Self::detect_wsl(),
            is_ssh: Self::detect_ssh(),
            is_modal: Self::detect_modal(),
            platform: Self::detect_platform(),
        }
    }

    fn detect_docker() -> bool {
        std::path::Path::new("/.dockerenv").exists()
            || std::fs::read_to_string("/proc/self/cgroup")
                .map(|s| s.contains("docker"))
                .unwrap_or(false)
    }

    fn detect_wsl() -> bool {
        std::fs::read_to_string("/proc/version")
            .map(|s| s.to_lowercase().contains("microsoft") || s.to_lowercase().contains("wsl"))
            .unwrap_or(false)
    }

    fn detect_ssh() -> bool {
        std::env::var("SSH_CLIENT").is_ok() || std::env::var("SSH_TTY").is_ok()
    }

    fn detect_modal() -> bool {
        std::env::var("MODAL_ENVIRONMENT").is_ok()
    }

    fn detect_platform() -> PlatformHint {
        #[cfg(target_os = "windows")]
        {
            if Self::detect_wsl() {
                PlatformHint::WSL
            } else {
                PlatformHint::Windows
            }
        }
        #[cfg(target_os = "linux")]
        {
            if Self::detect_docker() {
                PlatformHint::Docker
            } else {
                PlatformHint::Linux
            }
        }
        #[cfg(target_os = "macos")]
        {
            PlatformHint::MacOS
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            PlatformHint::Unknown
        }
    }

    /// Gera hints para o system prompt baseado no ambiente.
    pub fn system_prompt_hints(info: &EnvironmentInfo) -> Vec<String> {
        let mut hints = Vec::new();

        match info.platform {
            PlatformHint::Windows => {
                hints.push("Windows: use cmd /C for shell commands".to_string());
                hints.push("Windows: paths use backslash \\".to_string());
            }
            PlatformHint::WSL => {
                hints.push("WSL: hybrid Windows/Linux environment".to_string());
                hints.push("WSL: prefer Linux tools, Windows paths when interopping".to_string());
            }
            PlatformHint::Linux => {
                hints.push("Linux: standard POSIX shell".to_string());
            }
            PlatformHint::MacOS => {
                hints.push("macOS: BSD tools, may differ from GNU".to_string());
            }
            PlatformHint::Docker => {
                hints.push("Docker: isolated container environment".to_string());
            }
            PlatformHint::Unknown => {}
        }

        if info.is_docker {
            hints.push("Running inside Docker container".to_string());
        }
        if info.is_ssh {
            hints.push("SSH session: no GUI access".to_string());
        }
        if info.is_modal {
            hints.push("Modal cloud environment".to_string());
        }

        hints
    }
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_info() {
        let info = EnvironmentProbe::detect();
        // Pelo menos a plataforma deve ser detectada
        assert_ne!(info.platform, PlatformHint::Unknown);
    }

    #[test]
    fn hints_for_windows() {
        let info = EnvironmentInfo {
            is_docker: false,
            is_wsl: false,
            is_ssh: false,
            is_modal: false,
            platform: PlatformHint::Windows,
        };
        let hints = EnvironmentProbe::system_prompt_hints(&info);
        assert!(hints.iter().any(|h| h.contains("Windows")));
    }

    #[test]
    fn hints_for_wsl() {
        let info = EnvironmentInfo {
            is_docker: false,
            is_wsl: true,
            is_ssh: false,
            is_modal: false,
            platform: PlatformHint::WSL,
        };
        let hints = EnvironmentProbe::system_prompt_hints(&info);
        assert!(hints.iter().any(|h| h.contains("WSL")));
    }

    #[test]
    fn hints_include_docker() {
        let info = EnvironmentInfo {
            is_docker: true,
            is_wsl: false,
            is_ssh: false,
            is_modal: false,
            platform: PlatformHint::Linux,
        };
        let hints = EnvironmentProbe::system_prompt_hints(&info);
        assert!(hints.iter().any(|h| h.contains("Docker")));
    }

    #[test]
    fn hints_include_ssh() {
        let info = EnvironmentInfo {
            is_docker: false,
            is_wsl: false,
            is_ssh: true,
            is_modal: false,
            platform: PlatformHint::Linux,
        };
        let hints = EnvironmentProbe::system_prompt_hints(&info);
        assert!(hints.iter().any(|h| h.contains("SSH")));
    }
}
