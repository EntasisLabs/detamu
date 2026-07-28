//! Stable domain model for Detamu.
//!
//! This crate intentionally knows nothing about databases, language servers,
//! process hosting, or downstream consumers.

mod avec;
mod identity;
mod observation;

pub use avec::{
    AutonomyWeights, AvecScores, AvecWeights, FrictionWeights, LogicWeights, StabilityWeights,
};
pub use identity::{GitOid, LanguageId, RepositoryId, RevisionId, SymbolId};
pub use observation::{
    AnalysisCoverage, AnalysisDiagnostic, AnalyzerProvenance, CodeSymbol, DependencyObservation,
    DependencyType, DiagnosticSeverity, NodeKind, NodeMetrics, ObservationBatch, SymbolObservation,
};
