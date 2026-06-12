//! ReasoningBudget — limites determinísticos para o serviço de raciocínio.
//!
//! Todo modo de raciocínio (CoT/ToT/ReAct/PAL) opera sob um budget explícito.
//! O budget é verificado pelo harness ANTES de cada chamada ao LLM — o modelo
//! nunca decide se pode continuar.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Motivo de esgotamento do budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetExceeded {
    Steps,
    Tokens,
    Cost,
    Timeout,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BudgetExceeded::Steps => write!(f, "max_steps excedido"),
            BudgetExceeded::Tokens => write!(f, "max_tokens excedido"),
            BudgetExceeded::Cost => write!(f, "max_cost_usd excedido"),
            BudgetExceeded::Timeout => write!(f, "timeout_sec excedido"),
        }
    }
}

/// Veredito da verificação de budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdict {
    Ok,
    Exceeded(BudgetExceeded),
}

/// Budget de raciocínio com consumo rastreado (serializável para auditoria).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningBudget {
    pub max_steps: u32,
    pub max_tokens: u64,
    pub max_cost_usd: f64,
    pub timeout_sec: u64,
    // ── consumo ──
    pub steps_used: u32,
    pub tokens_used: u64,
    pub cost_used_usd: f64,
    /// Epoch (segundos) do início da sessão de raciocínio. 0 = não iniciado.
    pub started_at_epoch: u64,
}

impl ReasoningBudget {
    pub fn new(max_steps: u32, max_tokens: u64, max_cost_usd: f64, timeout_sec: u64) -> Self {
        Self {
            max_steps,
            max_tokens,
            max_cost_usd,
            timeout_sec,
            steps_used: 0,
            tokens_used: 0,
            cost_used_usd: 0.0,
            started_at_epoch: 0,
        }
    }

    /// Budget padrão conservador: 16 passos, 32k tokens, US$ 1.00, 120 s.
    pub fn default_budget() -> Self {
        Self::new(16, 32_000, 1.0, 120)
    }

    fn now_epoch() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Marca o início da contagem de timeout.
    pub fn start(&mut self) {
        if self.started_at_epoch == 0 {
            self.started_at_epoch = Self::now_epoch();
        }
    }

    /// Consome um passo de raciocínio.
    pub fn consume_step(&mut self) {
        self.steps_used += 1;
    }

    /// Registra uso de tokens e custo de uma chamada LLM.
    pub fn record_usage(&mut self, tokens: u64, cost_usd: f64) {
        self.tokens_used += tokens;
        self.cost_used_usd += cost_usd;
    }

    /// Verifica todos os limites. Chamado pelo harness antes de cada passo.
    pub fn check(&self) -> BudgetVerdict {
        if self.steps_used >= self.max_steps {
            return BudgetVerdict::Exceeded(BudgetExceeded::Steps);
        }
        if self.tokens_used >= self.max_tokens {
            return BudgetVerdict::Exceeded(BudgetExceeded::Tokens);
        }
        if self.cost_used_usd >= self.max_cost_usd {
            return BudgetVerdict::Exceeded(BudgetExceeded::Cost);
        }
        if self.started_at_epoch > 0 {
            let elapsed = Self::now_epoch().saturating_sub(self.started_at_epoch);
            if elapsed >= self.timeout_sec {
                return BudgetVerdict::Exceeded(BudgetExceeded::Timeout);
            }
        }
        BudgetVerdict::Ok
    }
}

impl Default for ReasoningBudget {
    fn default() -> Self {
        Self::default_budget()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_novo_esta_ok() {
        let b = ReasoningBudget::default_budget();
        assert_eq!(b.check(), BudgetVerdict::Ok);
    }

    #[test]
    fn max_steps_esgota() {
        let mut b = ReasoningBudget::new(2, 1000, 1.0, 60);
        b.consume_step();
        assert_eq!(b.check(), BudgetVerdict::Ok);
        b.consume_step();
        assert_eq!(b.check(), BudgetVerdict::Exceeded(BudgetExceeded::Steps));
    }

    #[test]
    fn max_tokens_esgota() {
        let mut b = ReasoningBudget::new(10, 100, 1.0, 60);
        b.record_usage(100, 0.0);
        assert_eq!(b.check(), BudgetVerdict::Exceeded(BudgetExceeded::Tokens));
    }

    #[test]
    fn max_cost_esgota() {
        let mut b = ReasoningBudget::new(10, 10_000, 0.5, 60);
        b.record_usage(10, 0.5);
        assert_eq!(b.check(), BudgetVerdict::Exceeded(BudgetExceeded::Cost));
    }

    #[test]
    fn timeout_zero_esgota_imediatamente_apos_start() {
        let mut b = ReasoningBudget::new(10, 10_000, 1.0, 0);
        b.start();
        assert_eq!(b.check(), BudgetVerdict::Exceeded(BudgetExceeded::Timeout));
    }

    #[test]
    fn sem_start_timeout_nao_conta() {
        let b = ReasoningBudget::new(10, 10_000, 1.0, 0);
        // started_at_epoch == 0 → timeout não verificado
        assert_eq!(b.check(), BudgetVerdict::Ok);
    }

    #[test]
    fn budget_serializa_para_auditoria() {
        let mut b = ReasoningBudget::default_budget();
        b.consume_step();
        b.record_usage(50, 0.01);
        let json = serde_json::to_value(&b).unwrap();
        assert_eq!(json["steps_used"], 1);
        assert_eq!(json["tokens_used"], 50);
    }
}
