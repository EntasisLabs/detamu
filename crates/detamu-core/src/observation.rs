use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

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
    pub fn merge(&mut self, other: Self) -> Result<(), BatchMismatch> {
        let mut entities = HashMap::with_capacity(self.entities.len());
        for (index, observation) in self.entities.iter().enumerate() {
            entities
                .entry(observation.entity.id.clone())
                .or_insert(index);
        }
        let mut relations = HashMap::with_capacity(self.relations.len());
        for (index, observation) in self.relations.iter().enumerate() {
            relations
                .entry(observation.relation.id.clone())
                .or_insert(index);
        }
        merge_indexed(self, &mut entities, &mut relations, other)
    }
}

/// Merges many observation batches while keeping entity and relation indexes.
///
/// Building the index once matters when analyzers reconcile a file at a time.
/// A linear scan of the growing snapshot is quadratic in repository size and
/// keeps both batches alive for the whole scan.
pub struct ObservationAccumulator {
    batch: ObservationBatch,
    entities: HashMap<EntityId, usize>,
    relations: HashMap<RelationId, usize>,
}

impl ObservationAccumulator {
    pub fn new(snapshot: SnapshotId) -> Self {
        Self {
            batch: ObservationBatch::empty(snapshot),
            entities: HashMap::new(),
            relations: HashMap::new(),
        }
    }

    pub fn set_coverage(&mut self, coverage: AnalysisCoverage) {
        self.batch.coverage = coverage;
    }

    pub fn push_provenance(&mut self, provenance: ObserverProvenance) {
        self.batch.provenance.push(provenance);
    }

    /// # Errors
    ///
    /// Returns [`BatchMismatch`] when `other` disagrees with observations
    /// already accumulated for the same entity or relation.
    pub fn merge(&mut self, other: ObservationBatch) -> Result<(), BatchMismatch> {
        merge_indexed(
            &mut self.batch,
            &mut self.entities,
            &mut self.relations,
            other,
        )
    }

    pub fn finish(self) -> ObservationBatch {
        self.batch
    }
}

fn merge_indexed(
    batch: &mut ObservationBatch,
    entities: &mut HashMap<EntityId, usize>,
    relations: &mut HashMap<RelationId, usize>,
    mut other: ObservationBatch,
) -> Result<(), BatchMismatch> {
    if batch.snapshot != other.snapshot {
        return Err(BatchMismatch::new("batches describe different snapshots"));
    }
    if batch.commit_mode != other.commit_mode {
        return Err(BatchMismatch::new("batches use different commit semantics"));
    }

    batch.provenance.append(&mut other.provenance);
    for observation in other.entities {
        if let Some(&index) = entities.get(&observation.entity.id) {
            merge_entity(&mut batch.entities[index], observation)?;
        } else {
            entities.insert(observation.entity.id.clone(), batch.entities.len());
            batch.entities.push(observation);
        }
    }
    for observation in other.relations {
        if let Some(&index) = relations.get(&observation.relation.id) {
            if batch.relations[index] != observation {
                return Err(BatchMismatch::new(format!(
                    "relation {} has conflicting observations",
                    observation.relation.id
                )));
            }
        } else {
            relations.insert(observation.relation.id.clone(), batch.relations.len());
            batch.relations.push(observation);
        }
    }
    batch.diagnostics.append(&mut other.diagnostics);
    batch.coverage = merge_coverage(batch.coverage, other.coverage);
    Ok(())
}

fn merge_entity(
    existing: &mut EntityObservation,
    incoming: EntityObservation,
) -> Result<(), BatchMismatch> {
    if existing.snapshot != incoming.snapshot
        || existing.entity.id != incoming.entity.id
        || existing.entity.model != incoming.entity.model
    {
        return Err(BatchMismatch::new(format!(
            "entity {} has conflicting identity",
            incoming.entity.id
        )));
    }
    if existing.entity.kind != incoming.entity.kind {
        if existing.entity.kind == "unknown" {
            existing.entity.kind.clone_from(&incoming.entity.kind);
        } else if incoming.entity.kind != "unknown" {
            return Err(BatchMismatch::new(format!(
                "entity {} has conflicting kinds {} and {}",
                incoming.entity.id, existing.entity.kind, incoming.entity.kind
            )));
        }
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
            return Err(BatchMismatch::new(format!(
                "entity {} has conflicting attribute {name}",
                existing.entity.id
            )));
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
                return Err(BatchMismatch::new(format!(
                    "entity {} has conflicting measurement {}",
                    existing.entity.id, measurement.name
                )));
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
                return Err(BatchMismatch::new(format!(
                    "entity {} has conflicting score {}",
                    existing.entity.id, score.dimension
                )));
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchMismatch {
    reason: String,
}

impl BatchMismatch {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for BatchMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl std::error::Error for BatchMismatch {}

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

    #[test]
    fn merge_prefers_a_concrete_kind_over_unknown() {
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        let mut left = ObservationBatch::empty(snapshot.clone());
        let mut uncertain = observed("syntax.complexity", 2.0);
        uncertain.entity.kind = "unknown".to_owned();
        left.entities.push(uncertain);
        let mut right = ObservationBatch::empty(snapshot);
        let mut concrete = observed("graph.incoming", 3.0);
        concrete.entity.kind = "method".to_owned();
        right.entities.push(concrete);

        left.merge(right).expect("merge concrete kind");

        assert_eq!(left.entities[0].entity.kind, "method");
    }

    #[test]
    fn merge_updates_the_matching_entity_among_neighbors() {
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        let mut left = ObservationBatch::empty(snapshot.clone());
        let mut first = observed("syntax.complexity", 1.0);
        first.entity.id = EntityId::new("first");
        let mut second = observed("syntax.complexity", 2.0);
        second.entity.id = EntityId::new("second");
        left.entities.extend([first, second]);
        let mut right = ObservationBatch::empty(snapshot);
        let mut enrichment = observed("graph.incoming", 4.0);
        enrichment.entity.id = EntityId::new("second");
        right.entities.push(enrichment);

        left.merge(right).expect("merge neighbor");

        assert_eq!(left.entities.len(), 2);
        assert_eq!(left.entities[0].measurements.len(), 1);
        assert_eq!(left.entities[1].measurements.len(), 2);
        assert_eq!(left.entities[1].measurements[1].name, "graph.incoming");
    }
}
