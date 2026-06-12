//! # arreio-autopoiesis
//!
//! Autopoietic Sustainability — agente como ecossistema auto-regenerativo.
//!
//! Implementa o loop MAPE-K (Monitor-Analyze-Plan-Execute-Knowledge) de
//! computação autonômica, combinado com autopoiese de Maturana & Varela.
//!
//! O ambiente perturba mas não controla.

pub mod autopoietic_system;
pub mod health_monitor;
pub mod mapek;
pub mod self_healing;

pub use autopoietic_system::*;
pub use health_monitor::*;
pub use mapek::*;
pub use self_healing::*;
