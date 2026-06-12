pub mod auto_compressor;
pub mod auto_lifecycle;
pub mod chat_consolidator;
pub mod chunker;
pub mod consolidator;
pub mod context_assembler;
pub mod context_collapse;
pub mod llm_summarizer;
pub mod context_compressor;
pub mod context_references;
pub mod engram_cache;
pub mod envelope;
pub mod episodic;
pub mod frozen_snapshot;
pub mod graph;
pub mod graph_rag;
pub mod help_contextual;
pub mod intent_classifier;
pub mod lifecycle;
pub mod meta_cognitive;
pub mod onboarding;
pub mod procedural;
pub mod project;
pub mod recall;
pub mod semantic;
pub mod session;
pub mod session_context;
pub mod sif;
pub mod trajectory;
pub mod transparent_session;

pub use auto_compressor::{AutoCompressResult, AutoCompressor};
pub use auto_lifecycle::AutoLifecycle;
pub use chat_consolidator::{ChatConsolidator, ConsolidationResult};
pub use chunker::{
    Chunk, ChunkPipeline, ChunkStore, Chunker, CodeChunker, FixedSizeChunker, MarkdownChunker,
    PowerLawOfPractice, ReasoningChunker, RecursiveChunker, SemanticChunker,
};
pub use consolidator::{ConsolidationReport, FlushPlan, MemoryConsolidator, TimelineRecorder};
pub use context_assembler::ContextAssembler;
pub use context_collapse::{ContextCollapser, Summarizer};
pub use llm_summarizer::LlmSummarizer;
pub use context_compressor::{
    CompressionResult, CompressorConfig, ContextCompressor, ContextMessage, ContextRole,
    ToolCallRef,
};
pub use context_references::{
    check_injection_budget, ContextReference, ContextReferenceParser, ContextReferenceResolver,
    ReferenceError,
};
pub use engram_cache::EngramCache;
pub use envelope::{MemoryEnvelope, MemoryType, ModalityRef, Scope};
pub use episodic::{EpisodicEvent, EpisodicMemory};
pub use frozen_snapshot::FrozenSnapshot;
pub use graph::GraphStore;
pub use graph_rag::{GraphRagPipeline, GraphRagResult, RagSource};
pub use help_contextual::HelpContextual;
pub use intent_classifier::{IntentClassifier, IntentResult, UserIntent};
pub use lifecycle::{LifecycleGovernance, MemoryState};
pub use meta_cognitive::{
    CognitiveBias, DetectedBias, ImprovementSuggestion, MetaCognitiveMonitor, MetaCostModel,
    OperationType, ReasoningLoop, ReasoningQuality, ReasoningStep, SemanticEntropy,
    TeiresiasExplainer,
};
pub use onboarding::{OnboardingWizard, UserProfile};
pub use procedural::{ProceduralMemory, ProductionRule};
pub use project::ProjectMemory;
pub use recall::{RecallPipeline, RecallResult};
pub use semantic::{SemanticConcept, SemanticMemory};
pub use session::{
    ChatMessage, ChatRole, Session, SessionContextBudget, SessionManager, SessionMode,
};
pub use session_context::{SessionAntiLoop, SessionContextFrame, SessionLifecycleState};
pub use sif::{SifAssembler, SifContextFrame};
pub use trajectory::{
    ToolStat, TrajectoryCompressor, TrajectoryMetadata, TrajectorySample, TrajectoryStorage,
};
pub use transparent_session::{ActiveSession, FriendlySession, TransparentSessionManager};
