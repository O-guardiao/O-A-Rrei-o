//! # arreio-ooda
//!
//! OODA-C Control Loop — substitui o loop ReAct por Observe-Orient-Decide-Act
//! com homeostase artificial, IG&C (Implicit Guidance and Control) e
//! variáveis essenciais de Ashby.
//!
//! Inspirado em: Boyd (1986), Ashby (1956), von Foerster (1974).

pub mod essential_variables;
pub mod flow_decision;
pub mod igc;
pub mod loop_engine;
pub mod pattern_classifier;
pub mod step_function;

pub use essential_variables::*;
pub use flow_decision::*;
pub use igc::*;
pub use loop_engine::*;
pub use pattern_classifier::*;
pub use step_function::*;
