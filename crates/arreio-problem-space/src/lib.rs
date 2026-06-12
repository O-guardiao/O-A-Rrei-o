//! # arreio-problem-space
//!
//! Problem Space Engine — implementa a Hipótese do Espaço de Problemas de Newell
//! e Simon (1982) com Universal Subgoaling de SOAR.
//!
//! Quatro tipos de impasse disparam submetas automáticas:
//! - state no-change
//! - operator tie
//! - operator conflict
//! - rejection

pub mod impasse;
pub mod operator;
pub mod problem_space;
pub mod universal_subgoaling;

pub use impasse::*;
pub use operator::*;
pub use problem_space::*;
pub use universal_subgoaling::*;
