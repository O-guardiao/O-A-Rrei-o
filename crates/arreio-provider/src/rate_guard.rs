use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Snapshot de rate-limit extraído dos headers HTTP de uma resposta.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub remaining_requests: Option<u32>,
    pub reset_timestamp: Option<u64>,
    pub retry_after: Option<u64>,
}

impl RateLimitSnapshot {
    /// Constrói snapshot a partir de headers HTTP (case-insensitive nas chaves).
    pub fn from_headers(headers: &HashMap<String, String>) -> Self {
        let mut snap = Self::default();
        for (k, v) in headers {
            let key = k.to_ascii_lowercase();
            match key.as_str() {
                "x-ratelimit-remaining-requests" => {
                    snap.remaining_requests = v.parse().ok();
                }
                "x-ratelimit-reset-requests" => {
                    if let Ok(ts) = v.parse::<u64>() {
                        snap.reset_timestamp = Some(ts);
                    }
                }
                "x-ratelimit-reset-requests-1h" => {
                    if let Ok(ts) = v.parse::<u64>() {
                        if snap.reset_timestamp.is_none() {
                            snap.reset_timestamp = Some(ts);
                        }
                    }
                }
                "retry-after" => {
                    if let Ok(secs) = v.parse::<u64>() {
                        snap.retry_after = Some(secs);
                        if snap.reset_timestamp.is_none() {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            snap.reset_timestamp = Some(now + secs);
                        }
                    }
                }
                _ => {}
            }
        }
        snap
    }
}

/// Estado persistente cross-session de rate-limit para um provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RateLimitState {
    pub provider: String,
    pub remaining_requests: Option<u32>,
    pub reset_timestamp: Option<u64>,
    pub last_known_bucket_near_exhausted: bool,
    pub is_tripped: bool,
}

impl RateLimitState {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            remaining_requests: None,
            reset_timestamp: None,
            last_known_bucket_near_exhausted: false,
            is_tripped: false,
        }
    }
}

/// Erro retornado quando o rate-limit guard bloqueia uma chamada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitError {
    Tripped {
        provider: String,
        reset_in_secs: u64,
    },
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitError::Tripped {
                provider,
                reset_in_secs,
            } => {
                write!(
                    f,
                    "RateLimit tripped para provider '{}', aguarde {}s",
                    provider, reset_in_secs
                )
            }
        }
    }
}

impl std::error::Error for RateLimitError {}

/// Guarda cross-session que evita retry amplification em rate-limits genuínos.
#[derive(Debug, Clone)]
pub struct RateLimitGuard {
    state_dir: PathBuf,
    states: Arc<Mutex<HashMap<String, RateLimitState>>>,
}

impl RateLimitGuard {
    /// Cria guard com diretório padrão: `~/.arreio/rate_limits` (ou `.arreio/rate_limits` no cwd).
    pub fn new() -> Self {
        let dir = Self::default_dir();
        Self::with_dir(dir)
    }

    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let _ = fs::create_dir_all(&dir);
        Self {
            state_dir: dir,
            states: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn default_dir() -> PathBuf {
        if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
            let p = PathBuf::from(home).join(".arreio/rate_limits");
            if p.parent().map(|d| d.exists()).unwrap_or(false) {
                return p;
            }
        }
        PathBuf::from(".arreio/rate_limits")
    }

    fn state_path(&self, provider: &str) -> PathBuf {
        self.state_dir.join(format!("{}.json", provider))
    }

    fn load(&self, provider: &str) -> RateLimitState {
        let path = self.state_path(provider);
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(mut state) = serde_json::from_str::<RateLimitState>(&data) {
                state.provider = provider.to_string();
                return state;
            }
        }
        RateLimitState::new(provider)
    }

    fn save(&self, state: &RateLimitState) {
        let path = self.state_path(&state.provider);
        let tmp = path.with_extension("tmp");
        if let Ok(mut f) = File::create(&tmp) {
            if serde_json::to_string_pretty(state)
                .map(|json| f.write_all(json.as_bytes()).is_ok())
                .unwrap_or(false)
            {
                let _ = f.flush();
                drop(f);
                let _ = fs::rename(&tmp, &path);
            }
        }
    }

    /// Verifica se o provider está bloqueado antes de bater na API.
    pub fn pre_flight_check(&self, provider: &str) -> Result<(), RateLimitError> {
        let mut cache = self.states.lock().unwrap();
        let state = cache
            .entry(provider.to_string())
            .or_insert_with(|| self.load(provider));

        if state.is_tripped {
            if let Some(reset) = state.reset_timestamp {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if reset > now {
                    return Err(RateLimitError::Tripped {
                        provider: provider.to_string(),
                        reset_in_secs: reset - now,
                    });
                } else {
                    state.is_tripped = false;
                    state.last_known_bucket_near_exhausted = false;
                }
            } else {
                return Err(RateLimitError::Tripped {
                    provider: provider.to_string(),
                    reset_in_secs: 0,
                });
            }
        }

        Ok(())
    }

    /// Registra uma resposta bem-sucedida, atualizando o estado com headers.
    pub fn record_success(&self, provider: &str, snapshot: Option<&RateLimitSnapshot>) {
        let mut cache = self.states.lock().unwrap();
        let state = cache
            .entry(provider.to_string())
            .or_insert_with(|| self.load(provider));

        if let Some(snap) = snapshot {
            state.remaining_requests = snap.remaining_requests;
            if let Some(ts) = snap.reset_timestamp {
                state.reset_timestamp = Some(ts);
            }
            if let Some(rem) = snap.remaining_requests {
                state.last_known_bucket_near_exhausted = rem <= 5;
            }
        }

        if state.is_tripped {
            if let Some(reset) = state.reset_timestamp {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now >= reset {
                    state.is_tripped = false;
                    state.last_known_bucket_near_exhausted = false;
                }
            }
        }

        self.save(state);
    }

    /// Registra um erro. Se for 429, tripa o breaker.
    pub fn record_error(&self, provider: &str, err: &anyhow::Error) {
        let err_str = err.to_string().to_ascii_lowercase();
        let is_429 = err_str.contains("429")
            || err_str.contains("too many requests")
            || err_str.contains("rate limited")
            || err_str.contains("rate limit");

        let mut cache = self.states.lock().unwrap();
        let state = cache
            .entry(provider.to_string())
            .or_insert_with(|| self.load(provider));

        if is_429 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let retry_after = Self::extract_retry_after(&err_str);
            let reset_ts = retry_after.map(|secs| now + secs).or(state.reset_timestamp);

            state.is_tripped = true;
            state.reset_timestamp = reset_ts;
            state.remaining_requests = Some(0);
        }

        self.save(state);
    }

    fn extract_retry_after(err_str: &str) -> Option<u64> {
        if let Some(pos) = err_str.find("retry-after") {
            let sub = &err_str[pos..];
            let num_start = sub.find(|c: char| c.is_ascii_digit())?;
            let num_str: String = sub[num_start..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return num_str.parse().ok();
        }
        if let Some(pos) = err_str.find("retry after") {
            let sub = &err_str[pos..];
            let num_start = sub.find(|c: char| c.is_ascii_digit())?;
            let num_str: String = sub[num_start..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return num_str.parse().ok();
        }
        None
    }

    /// Retorna o estado atual em memória (para testes/observabilidade).
    pub fn current_state(&self, provider: &str) -> Option<RateLimitState> {
        let cache = self.states.lock().unwrap();
        cache
            .get(provider)
            .cloned()
            .or_else(|| Some(self.load(provider)))
    }

    /// Força o reset manual de um provider (útil para testes e rollback).
    pub fn reset_provider(&self, provider: &str) {
        let mut cache = self.states.lock().unwrap();
        let state = cache
            .entry(provider.to_string())
            .or_insert_with(|| self.load(provider));
        state.is_tripped = false;
        state.reset_timestamp = None;
        state.remaining_requests = None;
        state.last_known_bucket_near_exhausted = false;
        self.save(state);
    }
}

impl Default for RateLimitGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_guard() -> RateLimitGuard {
        let dir = tempfile::tempdir().unwrap();
        RateLimitGuard::with_dir(dir.path())
    }

    #[test]
    fn pre_flight_allows_when_not_tripped() {
        let guard = temp_guard();
        assert!(guard.pre_flight_check("openai").is_ok());
    }

    #[test]
    fn pre_flight_blocks_when_tripped() {
        let guard = temp_guard();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        {
            let mut cache = guard.states.lock().unwrap();
            let mut state = RateLimitState::new("openai");
            state.is_tripped = true;
            state.reset_timestamp = Some(now + 300);
            cache.insert("openai".to_string(), state.clone());
            guard.save(&state);
        }
        assert!(guard.pre_flight_check("openai").is_err());
    }

    #[test]
    fn pre_flight_allows_after_reset_expires() {
        let guard = temp_guard();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        {
            let mut cache = guard.states.lock().unwrap();
            let mut state = RateLimitState::new("openai");
            state.is_tripped = true;
            state.reset_timestamp = Some(now - 1);
            cache.insert("openai".to_string(), state.clone());
            guard.save(&state);
        }
        assert!(guard.pre_flight_check("openai").is_ok());
    }

    #[test]
    fn record_success_updates_remaining() {
        let guard = temp_guard();
        let snap = RateLimitSnapshot {
            remaining_requests: Some(3),
            reset_timestamp: Some(9999999999),
            retry_after: None,
        };
        guard.record_success("anthropic", Some(&snap));
        let state = guard.current_state("anthropic").unwrap();
        assert_eq!(state.remaining_requests, Some(3));
        assert!(state.last_known_bucket_near_exhausted);
    }

    #[test]
    fn record_success_does_not_trip() {
        let guard = temp_guard();
        let snap = RateLimitSnapshot {
            remaining_requests: Some(100),
            reset_timestamp: None,
            retry_after: None,
        };
        guard.record_success("openai", Some(&snap));
        assert!(!guard.current_state("openai").unwrap().is_tripped);
    }

    #[test]
    fn record_error_trips_on_429() {
        let guard = temp_guard();
        let err = anyhow::anyhow!("HTTP 429: Too Many Requests");
        guard.record_error("openai", &err);
        let state = guard.current_state("openai").unwrap();
        assert!(state.is_tripped);
        assert_eq!(state.remaining_requests, Some(0));
    }

    #[test]
    fn amplification_prevention() {
        let guard = temp_guard();
        let err = anyhow::anyhow!("HTTP 429: rate limited, retry-after=120");
        guard.record_error("openai", &err);
        assert!(guard.current_state("openai").unwrap().is_tripped);

        let result = guard.pre_flight_check("openai");
        assert!(result.is_err());
        if let Err(RateLimitError::Tripped {
            provider,
            reset_in_secs,
        }) = result
        {
            assert_eq!(provider, "openai");
            assert!(reset_in_secs > 0);
        } else {
            panic!("esperado RateLimitError::Tripped");
        }
    }

    #[test]
    fn parse_headers_full() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-ratelimit-remaining-requests".to_string(),
            "0".to_string(),
        );
        headers.insert(
            "x-ratelimit-reset-requests".to_string(),
            "1234567890".to_string(),
        );
        headers.insert("retry-after".to_string(), "60".to_string());

        let snap = RateLimitSnapshot::from_headers(&headers);
        assert_eq!(snap.remaining_requests, Some(0));
        assert_eq!(snap.reset_timestamp, Some(1234567890));
        assert_eq!(snap.retry_after, Some(60));
    }

    #[test]
    fn parse_headers_1h_fallback() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-ratelimit-reset-requests-1h".to_string(),
            "1234567890".to_string(),
        );

        let snap = RateLimitSnapshot::from_headers(&headers);
        assert_eq!(snap.reset_timestamp, Some(1234567890));
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let guard = RateLimitGuard::with_dir(dir.path());
        let snap = RateLimitSnapshot {
            remaining_requests: Some(2),
            reset_timestamp: Some(12345),
            retry_after: None,
        };
        guard.record_success("ollama", Some(&snap));

        let guard2 = RateLimitGuard::with_dir(dir.path());
        let state = guard2.current_state("ollama").unwrap();
        assert_eq!(state.remaining_requests, Some(2));
        assert_eq!(state.reset_timestamp, Some(12345));
        assert!(state.last_known_bucket_near_exhausted);
    }
}
