//! Stable, world-model-agnostic kernel for Detamu.
//!
//! This crate intentionally knows nothing about code, tickets, databases,
//! process hosting, or downstream consumers.

mod identity;
mod observation;

pub use identity::{
    EntityId, ModelId, RelationId, ScoreModelId, SnapshotId, SnapshotVersion, WorldId,
};
pub use observation::{
    AnalysisCoverage, AnalysisDiagnostic, Attributes, BatchMismatch, CommitMode,
    DiagnosticSeverity, Entity, EntityObservation, Measurement, ObservationBatch,
    ObserverProvenance, Relation, RelationObservation, Score,
};
