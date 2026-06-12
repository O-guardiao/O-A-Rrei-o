/// Configurações globais do Arreio.

use std::time::Duration;

/// Valor padrão do modelo quando a variável de ambiente não está definida.
pub const DEFAULT_MODEL_STR: &str = "gemma4";

/// Retorna o modelo padrão para inferência.
/// Respeita a variável de ambiente `ARREIO_DEFAULT_MODEL`;
/// caso ausente, retorna [`DEFAULT_MODEL_STR`].
pub fn default_model() -> String {
    std::env::var("ARREIO_DEFAULT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_STR.to_string())
}

/// Lê uma variável de ambiente obrigatória.
/// Retorna `Err` com mensagem descritiva caso a variável esteja ausente ou vazia.
pub fn require_env(name: &str) -> anyhow::Result<String> {
    let val = std::env::var(name)?;
    if val.trim().is_empty() {
        anyhow::bail!("variável de ambiente {} está definida mas vazia", name);
    }
    Ok(val)
}

/// Lê uma variável de ambiente opcional.
/// Retorna `None` se ausente ou vazia.
pub fn optional_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

/// Configurações centralizadas de timeout para todo o sistema.
/// Evita magic numbers espalhados pelo workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArreioConfig {
    /// Timeout para chamadas LLM (chat/completion).
    pub llm_timeout: Duration,
    /// Timeout para chamadas de embedding.
    pub embed_timeout: Duration,
    /// Timeout para validação de código (hypervisor).
    pub validation_timeout: Duration,
    /// Timeout para conexões de gateway/bridge.
    pub gateway_timeout: Duration,
    /// Timeout para operações de MCP/A2A.
    pub mcp_timeout: Duration,
    /// Timeout para operações de arquivo e git.
    pub io_timeout: Duration,
    /// Número máximo de retries para chamadas LLM.
    pub llm_max_retries: u32,
    /// Budget padrão de iterações FSM.
    pub fsm_default_budget: u32,
    /// Intervalo entre execuções do Refiner (em nós concluídos).
    pub refiner_interval: u32,
    /// Tamanho máximo de output para delivery (caracteres).
    pub delivery_max_chars: usize,
}

impl ArreioConfig {
    /// Carrega configurações do ambiente, aplicando defaults seguros.
    pub fn from_env() -> Self {
        Self {
            llm_timeout: Self::parse_duration_secs("ARREIO_LLM_TIMEOUT_SECS", 60),
            embed_timeout: Self::parse_duration_secs("ARREIO_EMBED_TIMEOUT_SECS", 30),
            validation_timeout: Self::parse_duration_secs("ARREIO_VALIDATION_TIMEOUT_SECS", 300),
            gateway_timeout: Self::parse_duration_secs("ARREIO_GATEWAY_TIMEOUT_SECS", 5),
            mcp_timeout: Self::parse_duration_secs("ARREIO_MCP_TIMEOUT_SECS", 30),
            io_timeout: Self::parse_duration_secs("ARREIO_IO_TIMEOUT_SECS", 10),
            llm_max_retries: Self::parse_u32("ARREIO_LLM_MAX_RETRIES", 3),
            fsm_default_budget: Self::parse_u32("ARREIO_FSM_BUDGET", 90),
            refiner_interval: Self::parse_u32("ARREIO_REFINER_INTERVAL", 10),
            delivery_max_chars: Self::parse_usize("ARREIO_DELIVERY_MAX_CHARS", 4000),
        }
    }

    fn parse_duration_secs(var: &str, default: u64) -> Duration {
        Duration::from_secs(
            std::env::var(var)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default),
        )
    }

    fn parse_u32(var: &str, default: u32) -> u32 {
        std::env::var(var)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }

    fn parse_usize(var: &str, default: usize) -> usize {
        std::env::var(var)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    }
}

impl Default for ArreioConfig {
    fn default() -> Self {
        Self::from_env()
    }
}
