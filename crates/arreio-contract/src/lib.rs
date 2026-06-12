pub mod contract;
pub mod dbc;
pub mod engine;
pub mod hoare;
pub mod nl2contract;
pub mod predicates;
pub mod specification;
pub mod vc_generator;

// APIs legadas — mantidas para compatibilidade.
pub use contract::{
    AcceptanceTestCase, Contract, ContractResult, ContractVerificationResult, ContractViolation,
    EvaluationContext, Predicate, PredicateEvaluator, TestType, ViolationType,
};
pub use engine::ContractEngine;
pub use nl2contract::NL2Contract;

// Novas APIs de Design by Contract + Hoare Logic + VC Generator.
pub use dbc::{Contract as DbCContract, Predicate as DbCPredicate};
pub use hoare::{HoareLogic, HoareTriple};
pub use specification::SpecificationStatement;
pub use vc_generator::{VcGenerator, VerificationCondition};
