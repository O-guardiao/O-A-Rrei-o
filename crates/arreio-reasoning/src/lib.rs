//! arreio-reasoning — Reasoning como Serviço Auditável (PVC-Q2.1).
//!
//! Traduz os padrões de raciocínio do mercado (CoT, ToT, ReAct, PAL) para a
//! arquitetura determinística do Arreio:
//!
//! - **O harness escolhe o modo** (`PromptMode` em `arreio-provider`) — nunca o LLM.
//! - **Cada passo é uma tupla auditável** com hash SHA-256 encadeado no
//!   Blackboard (`ReasoningLedger`), à prova de adulteração.
//! - **Budget explícito** (`ReasoningBudget`: max_steps, max_tokens,
//!   max_cost_usd, timeout_sec) verificado ANTES de cada chamada.
//! - **ReAct vira estados FSM explícitos** (`ReasoningThought` →
//!   `ReasoningAction` → `ReasoningObservation` em `arreio-fsm`), não um loop
//!   opaco do agente.
//! - **Ações nunca são executadas livremente**: o LLM propõe, o
//!   `ActionExecutor` do chamador (ToolRegistry sob policy) executa.
//!
//! Síncrono, stateless, sem tokio — alinhado a ADR-0001/ADR-0002.

pub mod budget;
pub mod ledger;
pub mod service;

pub use budget::{BudgetExceeded, BudgetVerdict, ReasoningBudget};
pub use ledger::{ReasoningLedger, ReasoningPhase, ReasoningStep, GENESIS_HASH};
pub use service::{
    ActionExecutor, DenyAllExecutor, ReasoningOutcome, ReasoningRequest, ReasoningService,
};
