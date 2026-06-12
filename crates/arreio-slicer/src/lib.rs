//! # arreio-slicer
//!
//! Program Slicing para context curation — reduz código ao conjunto mínimo
//! de instruções relevantes para um critério dado.
//!
//! Inspirado em: Weiser (1979).
//!
//! Tipos de slice:
//! - Backward: instruções que influenciam o critério
//! - Forward: instruções influenciadas pelo critério

pub mod backward_slice;
pub mod context_curation;
pub mod criterion;
pub mod forward_slice;
pub mod slicer;

pub use backward_slice::*;
pub use context_curation::*;
pub use criterion::*;
pub use forward_slice::*;
pub use slicer::*;
