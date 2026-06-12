//! # arreio-eqsat
//!
//! Equality Saturation — otimização via e-graphs e reescrita não-destrutiva.
//!
//! Unifica supercompilação, partial evaluation e otimização universal.
//!
//! Inspirado em: egg (Wang et al., POPL 2021), Turchin (1970s).

pub mod egraph;
pub mod language;
pub mod rewrite_rule;
pub mod saturation_engine;

pub use egraph::*;
pub use language::*;
pub use rewrite_rule::*;
pub use saturation_engine::*;
