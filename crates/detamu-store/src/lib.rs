//! Persistence contract for Detamu.
//!
//! `SurrealDB` is the intended production backend. The in-memory store is both a
//! useful embedding option and the behavioral reference for backend tests.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use detamu_core::{EntityId, EntityObservation, ObservationBatch, RelationObservation, SnapshotId};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationDirection {
    Incoming,
    Outgoing,
    Both,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store rejected an inconsistent observation batch: {0}")]
    InconsistentBatch(String),
    #[error("store operation failed: {0}")]
    Backend(String),
}

#[async_trait]
pub trait DetamuStore: Send + Sync {
    /// Applies the batch's explicit commit semantics atomically.
    ///
    /// `ReplaceSnapshot` is idempotent; entities or relations omitted from a
    /// later batch for the same snapshot are removed.
    async fn ingest(&self, batch: ObservationBatch) -> Result<(), StoreError>;

    async fn entity(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
    ) -> Result<Option<EntityObservation>, StoreError>;

    async fn relations(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
        direction: RelationDirection,
    ) -> Result<Vec<RelationObservation>, StoreError>;
}

/// Validates invariants shared by every storage backend.
///
/// # Errors
///
/// Returns [`StoreError::InconsistentBatch`] when observations cross snapshot
/// boundaries, identifiers are duplicated, relation endpoints are absent, or
/// numeric observations cannot be represented safely.
pub fn validate_batch(batch: &ObservationBatch) -> Result<(), StoreError> {
    if batch
        .entities
        .iter()
        .any(|observation| observation.snapshot != batch.snapshot)
        || batch
            .relations
            .iter()
            .any(|observation| observation.snapshot != batch.snapshot)
    {
        return inconsistent("an observation belongs to a different snapshot");
    }

    let mut entities = HashSet::with_capacity(batch.entities.len());
    for observation in &batch.entities {
        if !entities.insert(&observation.entity.id) {
            return inconsistent("an entity identifier appears more than once");
        }
        if observation
            .measurements
            .iter()
            .any(|measurement| !measurement.value.is_finite())
        {
            return inconsistent("an entity contains a non-finite measurement");
        }
        if observation
            .scores
            .iter()
            .any(|score| !normalized(score.value))
        {
            return inconsistent("an entity contains a non-normalized score");
        }
    }

    let mut relations = HashSet::with_capacity(batch.relations.len());
    for observation in &batch.relations {
        if !entities.contains(&observation.relation.from)
            || !entities.contains(&observation.relation.to)
        {
            return inconsistent("a relation endpoint is absent from the snapshot");
        }
        if !normalized(observation.weight) {
            return inconsistent("a relation weight is outside 0.0..=1.0");
        }
        if !relations.insert(&observation.relation.id) {
            return inconsistent("a relation identifier appears more than once");
        }
    }

    Ok(())
}

fn inconsistent<T>(message: &str) -> Result<T, StoreError> {
    Err(StoreError::InconsistentBatch(message.to_owned()))
}

fn normalized(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[derive(Debug, Default)]
struct MemoryState {
    entities: HashMap<(SnapshotId, EntityId), EntityObservation>,
    relations: Vec<RelationObservation>,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryStore {
    state: Arc<RwLock<MemoryState>>,
}

#[async_trait]
impl DetamuStore for InMemoryStore {
    async fn ingest(&self, batch: ObservationBatch) -> Result<(), StoreError> {
        validate_batch(&batch)?;

        let mut state = self.state.write().await;
        state
            .entities
            .retain(|(snapshot, _), _| snapshot != &batch.snapshot);
        for observation in batch.entities {
            let key = (batch.snapshot.clone(), observation.entity.id.clone());
            state.entities.insert(key, observation);
        }

        state
            .relations
            .retain(|relation| relation.snapshot != batch.snapshot);
        state.relations.extend(batch.relations);
        Ok(())
    }

    async fn entity(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
    ) -> Result<Option<EntityObservation>, StoreError> {
        Ok(self
            .state
            .read()
            .await
            .entities
            .get(&(snapshot.clone(), entity.clone()))
            .cloned())
    }

    async fn relations(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
        direction: RelationDirection,
    ) -> Result<Vec<RelationObservation>, StoreError> {
        let relations = self
            .state
            .read()
            .await
            .relations
            .iter()
            .filter(|observation| {
                observation.snapshot == *snapshot
                    && match direction {
                        RelationDirection::Incoming => observation.relation.to == *entity,
                        RelationDirection::Outgoing => observation.relation.from == *entity,
                        RelationDirection::Both => {
                            observation.relation.from == *entity
                                || observation.relation.to == *entity
                        }
                    }
            })
            .cloned()
            .collect();
        Ok(relations)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use detamu_core::{Entity, ModelId, SnapshotVersion, WorldId};

    use super::*;

    #[tokio::test]
    async fn stores_entities_by_exact_snapshot() {
        let store = InMemoryStore::default();
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        let entity_id = EntityId::new("entity:run");
        let observation = EntityObservation {
            snapshot: snapshot.clone(),
            entity: Entity {
                id: entity_id.clone(),
                model: ModelId::new("test"),
                kind: "item".to_owned(),
                label: "run".to_owned(),
            },
            attributes: BTreeMap::default(),
            measurements: Vec::new(),
            scores: Vec::new(),
        };
        let mut batch = ObservationBatch::empty(snapshot.clone());
        batch.entities.push(observation.clone());

        store.ingest(batch).await.expect("batch should ingest");
        assert_eq!(
            store.entity(&snapshot, &entity_id).await.expect("lookup"),
            Some(observation)
        );
    }

    #[tokio::test]
    async fn reindex_replaces_stale_snapshot_data() {
        let store = InMemoryStore::default();
        let snapshot = SnapshotId::new(WorldId::new("world"), SnapshotVersion::new("v1"));
        let stale_id = EntityId::new("stale");
        let mut original = ObservationBatch::empty(snapshot.clone());
        original.entities.push(EntityObservation {
            snapshot: snapshot.clone(),
            entity: Entity {
                id: stale_id.clone(),
                model: ModelId::new("test"),
                kind: "item".to_owned(),
                label: "stale".to_owned(),
            },
            attributes: BTreeMap::default(),
            measurements: Vec::new(),
            scores: Vec::new(),
        });
        store.ingest(original).await.expect("initial ingest");

        store
            .ingest(ObservationBatch::empty(snapshot.clone()))
            .await
            .expect("replacement ingest");
        assert_eq!(
            store.entity(&snapshot, &stale_id).await.expect("lookup"),
            None
        );
    }
}
