//! # arreio-recovery
//!
//! Recovery Block Multi-Model — tolerância a falhas via diversidade de LLMs.
//!
//! Sintaxe: ensure <acceptance_test> by <primary> else by <alt1> else by <alt2> else error
//!
//! Inspirado em: Randell (1974), Avizienis (1985).

pub mod acceptance_test;
pub mod multi_model;
pub mod provider_integration;
pub mod recovery_block;
pub mod recovery_cache;

pub use acceptance_test::*;
pub use multi_model::*;
pub use provider_integration::*;
pub use recovery_block::*;
pub use recovery_cache::*;
