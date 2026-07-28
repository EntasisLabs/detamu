use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EntityId, ModelId, RelationId, ScoreModelId, SnapshotId};

pub type Attributes = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserverProvenance {
    pub observer: String,
    pub version: String,
    pub configuration_digest: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub model: ModelId,
    pub kind: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub name: String,
    pub value: f64,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub model: ScoreModelId,
    pub version: u32,
    pub dimension: String,
    /// Normalized to `0.0..=1.0`.
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityObservation {
    pub snapshot: SnapshotId,
    pub entity: Entity,
    pub attributes: Attributes,
    pub measurements: Vec<Measurement>,
    pub scores: Vec<Score>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub id: RelationId,
    pub model: ModelId,
    pub kind: String,
    pub from: EntityId,
    pub to: EntityId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationObservation {
    pub snapshot: SnapshotId,
    pub relation: Relation,
    pub weight: f64,
    pub attributes: Attributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisCoverage {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub severity: DiagnosticSeverity,
    pub observer: String,
    pub message: String,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitMode {
    /// Atomically replace the complete contents of one immutable snapshot.
    ReplaceSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationBatch {
    pub snapshot: SnapshotId,
    pub commit_mode: CommitMode,
    pub provenance: Vec<ObserverProvenance>,
    pub coverage: AnalysisCoverage,
    pub entities: Vec<EntityObservation>,
    pub relations: Vec<RelationObservation>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

impl ObservationBatch {
    pub fn empty(snapshot: SnapshotId) -> Self {
        Self {
            snapshot,
            commit_mode: CommitMode::ReplaceSnapshot,
            provenance: Vec::new(),
            coverage: AnalysisCoverage::Unavailable,
            entities: Vec::new(),
            relations: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Adds observations from another observer to this batch.
    ///
    /// # Errors
    ///
    /// Returns [`BatchMismatch`] when the batches describe different snapshots
    /// or use different commit semantics.
    pub fn merge(&mut self, mut other: Self) -> Result<(), BatchMismatch> {
        if self.snapshot != other.snapshot || self.commit_mode != other.commit_mode {
            return Err(BatchMismatch);
        }

        self.provenance.append(&mut other.provenance);
        self.entities.append(&mut other.entities);
        self.relations.append(&mut other.relations);
        self.diagnostics.append(&mut other.diagnostics);
        self.coverage = merge_coverage(self.coverage, other.coverage);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchMismatch;

fn merge_coverage(left: AnalysisCoverage, right: AnalysisCoverage) -> AnalysisCoverage {
    use AnalysisCoverage::{Complete, Partial, Unavailable};

    match (left, right) {
        (Unavailable, Unavailable) => Unavailable,
        (Complete, Complete) => Complete,
        _ => Partial,
    }
}
