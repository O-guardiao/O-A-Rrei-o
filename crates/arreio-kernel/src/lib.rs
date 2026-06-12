pub mod blackboard;
pub mod config;
pub mod stigmergy;
pub mod trajectory;
pub mod variety_engine;
pub mod vector;
pub mod vector_hnsw;
pub mod vsm;

pub mod blackboard_json;
pub mod persistence;

#[cfg(feature = "sqlite")]
pub mod blackboard_sqlite;

pub use blackboard::Blackboard;
pub use config::{default_model, require_env, optional_env, ArreioConfig, DEFAULT_MODEL_STR};
pub use stigmergy::*;
pub use trajectory::{ApprovalDecision, ContractViolation, HitlStatus, HumanDecision, TrajectoryEntry, TrajectoryResult, TrajectoryStore, ViolationType};
pub use variety_engine::VarietyEngine;
pub use vector::{
    active_vector_backend, cosine_similarity, LinearBackend, VectorBackend, VectorEntry, VectorHit,
};
pub use vector_hnsw::HnswBackend;
pub use vsm::*;

pub use blackboard_json::JsonBlackboard;
pub use persistence::{PersistentStorage, StorageOp};

#[cfg(feature = "sqlite")]
pub use blackboard_sqlite::SqliteBlackboard;
