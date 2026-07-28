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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidenceProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceProvenance {
    pub observer: String,
    /// Relative trust in this evidence in the inclusive range `0.0..=1.0`.
    pub confidence: f64,
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
        for observation in other.entities {
            if let Some(existing) = self
                .entities
                .iter_mut()
                .find(|existing| existing.entity.id == observation.entity.id)
            {
                merge_entity(existing, observation)?;
            } else {
                self.entities.push(observation);
            }
        }
        for observation in other.relations {
            if let Some(existing) = self
                .relations
                .iter()
                .find(|existing| existing.relation.id == observation.relation.id)
            {
                if existing != &observation {
                    return Err(BatchMismatch);
                }
            } else {
                self.relations.push(observation);
            }
        }
        self.diagnostics.append(&mut other.diagnostics);
        self.coverage = merge_coverage(self.coverage, other.coverage);
        Ok(())
    }
}

fn merge_entity(
    existing: &mut EntityObservation,
    incoming: EntityObservation,
) -> Result<(), BatchMismatch> {
    if existing.snapshot != incoming.snapshot
        || existing.entity.id != incoming.entity.id
        || existing.entity.model != incoming.entity.model
        || existing.entity.kind != incoming.entity.kind
    {
        return Err(BatchMismatch);
    }
    if preferred_label(&incoming.entity.label, &existing.entity.label) {
        existing.entity.label = incoming.entity.label;
    }
    for (name, value) in incoming.attributes {
        if existing
            .attributes
            .get(&name)
            .is_some_and(|current| current != &value)
        {
            return Err(BatchMismatch);
        }
        existing.attributes.insert(name, value);
    }
    for measurement in incoming.measurements {
        if let Some(current) = existing.measurements.iter().find(|current| {
            current.name == measurement.name
                && current.evidence.as_ref().map(|evidence| &evidence.observer)
                    == measurement
                        .evidence
                        .as_ref()
                        .map(|evidence| &evidence.observer)
        }) {
            if current != &measurement {
                return Err(BatchMismatch);
            }
        } else {
            existing.measurements.push(measurement);
        }
    }
    for score in incoming.scores {
        if let Some(current) = existing.scores.iter().find(|current| {
            current.model == score.model
                && current.version == score.version
                && current.dimension == score.dimension
        }) {
            if current != &score {
                return Err(BatchMismatch);
            }
        } else {
            existing.scores.push(score);
        }
    }
    Ok(())
}

fn preferred_label(candidate: &str, current: &str) -> bool {
    let qualification = |label: &str| label.matches("::").count() + label.matches('.').count();
    qualification(candidate)
        .cmp(&qualification(current))
        .then_with(|| candidate.len().cmp(&current.len()))
        .then_with(|| current.cmp(candidate))
        .is_gt()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SnapshotVersion, WorldId};

    fn observed(name: &str, value: f64) -> EntityObservation {
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        EntityObservation {
            snapshot,
            entity: Entity {
                id: EntityId::new("entity"),
                model: ModelId::new("model"),
                kind: "item".to_owned(),
                label: "Item".to_owned(),
            },
            attributes: Attributes::new(),
            measurements: vec![Measurement {
                name: name.to_owned(),
                value,
                unit: None,
                evidence: None,
            }],
            scores: Vec::new(),
        }
    }

    #[test]
    fn merge_enriches_the_same_entity() {
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        let mut left = ObservationBatch::empty(snapshot.clone());
        left.entities.push(observed("syntax.complexity", 2.0));
        let mut right = ObservationBatch::empty(snapshot);
        right.entities.push(observed("graph.incoming", 3.0));
        left.merge(right).expect("merge enrichment");
        assert_eq!(left.entities.len(), 1);
        assert_eq!(left.entities[0].measurements.len(), 2);
    }

    #[test]
    fn merge_rejects_conflicting_evidence() {
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        let mut left = ObservationBatch::empty(snapshot.clone());
        left.entities.push(observed("syntax.complexity", 2.0));
        let mut right = ObservationBatch::empty(snapshot);
        right.entities.push(observed("syntax.complexity", 3.0));
        assert!(left.merge(right).is_err());
    }
}
