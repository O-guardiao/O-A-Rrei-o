use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Estado de um circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,   // Normal — requests passam
    Open,     // Falhou muito — requests são rejeitadas imediatamente
    HalfOpen, // Testando se recuperou
}

/// Circuit breaker para chamadas ao provider LLM.
pub struct CircuitBreaker {
    state: Arc<Mutex<CircuitStateInner>>,
    failure_threshold: u32,
    recovery_timeout: Duration,
    half_open_max_calls: u32,
}

struct CircuitStateInner {
    state: CircuitState,
    failures: u32,
    last_failure: Option<Instant>,
    half_open_calls: u32,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, recovery_timeout_secs: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(CircuitStateInner {
                state: CircuitState::Closed,
                failures: 0,
                last_failure: None,
                half_open_calls: 0,
            })),
            failure_threshold,
            recovery_timeout: Duration::from_secs(recovery_timeout_secs),
            half_open_max_calls: 3,
        }
    }

    /// Executa uma operação protegida pelo circuit breaker.
    pub fn call<F, T>(&self, f: F) -> Result<T, CircuitError>
    where
        F: FnOnce() -> anyhow::Result<T>,
    {
        self.call_with_error(f).map_err(|(c, _)| c)
    }

    /// Executa uma operação protegida, preservando o erro original em caso de falha.
    pub fn call_with_error<F, T>(&self, f: F) -> Result<T, (CircuitError, anyhow::Error)>
    where
        F: FnOnce() -> anyhow::Result<T>,
    {
        {
            let mut inner = self.state.lock().unwrap();
            match inner.state {
                CircuitState::Open => {
                    if let Some(last) = inner.last_failure {
                        if last.elapsed() >= self.recovery_timeout {
                            inner.state = CircuitState::HalfOpen;
                            inner.half_open_calls = 0;
                        } else {
                            return Err((
                                CircuitError::Open,
                                anyhow::anyhow!("circuit breaker aberto"),
                            ));
                        }
                    }
                }
                CircuitState::HalfOpen => {
                    if inner.half_open_calls >= self.half_open_max_calls {
                        return Err((
                            CircuitError::Open,
                            anyhow::anyhow!("circuit breaker aberto"),
                        ));
                    }
                    inner.half_open_calls += 1;
                }
                CircuitState::Closed => {}
            }
        }

        match f() {
            Ok(val) => {
                let mut inner = self.state.lock().unwrap();
                if inner.state == CircuitState::HalfOpen {
                    inner.state = CircuitState::Closed;
                }
                inner.failures = 0;
                Ok(val)
            }
            Err(e) => {
                let mut inner = self.state.lock().unwrap();
                inner.failures += 1;
                inner.last_failure = Some(Instant::now());
                if inner.failures >= self.failure_threshold {
                    inner.state = CircuitState::Open;
                }
                Err((CircuitError::Failure, e))
            }
        }
    }

    pub fn current_state(&self) -> CircuitState {
        self.state.lock().unwrap().state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitError {
    Open,    // circuit aberto
    Failure, // falha na execução
}

impl std::fmt::Display for CircuitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitError::Open => write!(f, "Circuit breaker está aberto — provedor indisponível"),
            CircuitError::Failure => write!(f, "Falha na chamada protegida pelo circuit breaker"),
        }
    }
}

impl std::error::Error for CircuitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_fecha_apos_sucesso() {
        let cb = CircuitBreaker::new(3, 60);
        let result = cb.call(|| Ok(42));
        assert_eq!(result.unwrap(), 42);
        assert_eq!(cb.current_state(), CircuitState::Closed);
    }

    #[test]
    fn circuit_abre_apos_falhas() {
        let cb = CircuitBreaker::new(2, 60);
        let _: Result<(), _> = cb.call(|| Err(anyhow::anyhow!("fail")));
        let _: Result<(), _> = cb.call(|| Err(anyhow::anyhow!("fail")));
        assert_eq!(cb.current_state(), CircuitState::Open);
        let result = cb.call(|| Ok(42));
        assert!(matches!(result, Err(CircuitError::Open)));
    }
}
