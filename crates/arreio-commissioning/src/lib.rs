//! arreio-commissioning — Self-Commissioning / Meta-PVC (PVC-Q3.3).
//!
//! O próprio sistema produz seus artefatos PVC a partir de evidências reais:
//!
//! - **StubDetector** (papel do Inspector): varredura estática de
//!   `todo!`/`unimplemented!`/TODO/FIXME — "incompleto oculto não pode";
//! - **BriefGenerator** (papel do Arquiteto): PROJECT_BRIEF.md validado e
//!   renderizado a partir de entrada estruturada;
//! - **ReportGenerator** (papel do Refiner): COMMISSIONING_REPORT.md a partir
//!   de evidências verificáveis (saída real de `cargo test`, fluxos, stubs),
//!   com decisão calculada deterministicamente;
//! - **SelfCommissioner**: orquestra a rodada e registra auditoria no
//!   Blackboard (`commissioning::last_run`).
//!
//! Deliberadamente SEM LLM: artefatos de comissionamento são evidência, não
//! prosa gerada. Os arquivos nascem com sufixo `.generated` — a promoção a
//! artefato oficial é decisão humana (HITL).

pub mod brief_generator;
pub mod report_generator;
pub mod self_commissioner;
pub mod stub_detector;

pub use brief_generator::{BriefGenerator, BriefInput, RiskItem, SuccessMetric};
pub use report_generator::{
    CommissioningDecision, EvidencePack, FlowEvidence, ReportGenerator, TestSummary,
};
pub use self_commissioner::{CommissioningArtifacts, SelfCommissioner};
pub use stub_detector::{StubDetector, StubFinding, StubKind, StubReport};
