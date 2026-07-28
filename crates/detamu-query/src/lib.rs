//! World-model-agnostic read facade for persisted Detamu snapshots.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::Arc,
};

use detamu_core::{
    Attributes, EntityId, EntityObservation, RelationId, RelationObservation, SnapshotId, WorldId,
};
use detamu_store::{DetamuStore, RelationDirection, SnapshotRecord, StoreError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const QUERY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum QueryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("entity {entity} does not exist in snapshot")]
    EntityNotFound { entity: EntityId },
    #[error("snapshot diff requires two versions of the same world")]
    DifferentWorlds,
    #[error("graph traversal max_nodes must be greater than zero")]
    InvalidNodeLimit,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EntityFilter {
    pub model: Option<String>,
    pub kind: Option<String>,
    pub label_contains: Option<String>,
    pub attributes: Attributes,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphRequest {
    pub root: EntityId,
    pub direction: RelationDirection,
    pub max_depth: u32,
    pub max_nodes: usize,
    #[serde(default)]
    pub relation_kinds: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraversedEntity {
    pub depth: u32,
    pub observation: EntityObservation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphTraversal {
    pub schema_version: u32,
    pub snapshot: SnapshotId,
    pub root: EntityId,
    pub direction: RelationDirection,
    pub nodes: Vec<TraversedEntity>,
    pub relations: Vec<RelationObservation>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityChange {
    pub id: EntityId,
    pub before: EntityObservation,
    pub after: EntityObservation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationChange {
    pub id: RelationId,
    pub before: RelationObservation,
    pub after: RelationObservation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDiff {
    pub schema_version: u32,
    pub from: SnapshotId,
    pub to: SnapshotId,
    pub added_entities: Vec<EntityObservation>,
    pub removed_entities: Vec<EntityObservation>,
    pub changed_entities: Vec<EntityChange>,
    pub added_relations: Vec<RelationObservation>,
    pub removed_relations: Vec<RelationObservation>,
    pub changed_relations: Vec<RelationChange>,
}

pub struct SnapshotQuery {
    store: Arc<dyn DetamuStore>,
}

impl SnapshotQuery {
    pub fn new(store: Arc<dyn DetamuStore>) -> Self {
        Self { store }
    }

    /// Returns metadata for one immutable snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot complete the query.
    pub async fn snapshot(
        &self,
        snapshot: &SnapshotId,
    ) -> Result<Option<SnapshotRecord>, QueryError> {
        Ok(self.store.snapshot(snapshot).await?)
    }

    /// Lists persisted snapshots, optionally restricted to one world.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot complete the query.
    pub async fn snapshots(
        &self,
        world: Option<&WorldId>,
    ) -> Result<Vec<SnapshotRecord>, QueryError> {
        Ok(self.store.snapshots(world).await?)
    }

    /// Resolves one entity by stable identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot complete the query.
    pub async fn entity(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
    ) -> Result<Option<EntityObservation>, QueryError> {
        Ok(self.store.entity(snapshot, entity).await?)
    }

    /// Finds entities matching every supplied filter.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot enumerate the snapshot.
    pub async fn find_entities(
        &self,
        snapshot: &SnapshotId,
        filter: &EntityFilter,
    ) -> Result<Vec<EntityObservation>, QueryError> {
        let label = filter
            .label_contains
            .as_ref()
            .map(|label| label.to_lowercase());
        let mut entities = self
            .store
            .entities(snapshot)
            .await?
            .into_iter()
            .filter(|observation| {
                filter.model.as_ref().is_none_or(|model| {
                    observation
                        .entity
                        .model
                        .as_str()
                        .eq_ignore_ascii_case(model)
                }) && filter
                    .kind
                    .as_ref()
                    .is_none_or(|kind| observation.entity.kind.eq_ignore_ascii_case(kind))
                    && label
                        .as_ref()
                        .is_none_or(|label| observation.entity.label.to_lowercase().contains(label))
                    && filter
                        .attributes
                        .iter()
                        .all(|(name, value)| observation.attributes.get(name) == Some(value))
            })
            .collect::<Vec<_>>();
        if let Some(limit) = filter.limit {
            entities.truncate(limit);
        }
        Ok(entities)
    }

    /// Traverses a bounded, cycle-safe relation neighborhood.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is absent, the bound is invalid, or the
    /// backing store cannot enumerate the snapshot.
    pub async fn traverse(
        &self,
        snapshot: &SnapshotId,
        request: &GraphRequest,
    ) -> Result<GraphTraversal, QueryError> {
        if request.max_nodes == 0 {
            return Err(QueryError::InvalidNodeLimit);
        }
        let entities = self
            .store
            .entities(snapshot)
            .await?
            .into_iter()
            .map(|observation| (observation.entity.id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        if !entities.contains_key(&request.root) {
            return Err(QueryError::EntityNotFound {
                entity: request.root.clone(),
            });
        }
        let relations = self.store.snapshot_relations(snapshot).await?;
        let mut depths = HashMap::from([(request.root.clone(), 0_u32)]);
        let mut queue = VecDeque::from([request.root.clone()]);
        let mut traversed_relations = BTreeSet::new();
        let mut truncated = false;
        while let Some(entity) = queue.pop_front() {
            let depth = depths[&entity];
            if depth >= request.max_depth {
                continue;
            }
            for relation in relations.iter().filter(|relation| {
                request.relation_kinds.is_empty()
                    || request.relation_kinds.contains(&relation.relation.kind)
            }) {
                let Some(next) = connected_entity(relation, &entity, request.direction) else {
                    continue;
                };
                if !entities.contains_key(next) {
                    continue;
                }
                if depths.len() >= request.max_nodes && !depths.contains_key(next) {
                    truncated = true;
                    continue;
                }
                traversed_relations.insert(relation.relation.id.clone());
                if !depths.contains_key(next) {
                    depths.insert(next.clone(), depth.saturating_add(1));
                    queue.push_back(next.clone());
                }
            }
        }
        let mut nodes = depths
            .into_iter()
            .filter_map(|(id, depth)| {
                entities
                    .get(&id)
                    .cloned()
                    .map(|observation| TraversedEntity { depth, observation })
            })
            .collect::<Vec<_>>();
        nodes.sort_by(|left, right| {
            left.depth.cmp(&right.depth).then_with(|| {
                left.observation
                    .entity
                    .id
                    .as_str()
                    .cmp(right.observation.entity.id.as_str())
            })
        });
        let relations = relations
            .into_iter()
            .filter(|relation| traversed_relations.contains(&relation.relation.id))
            .collect();
        Ok(GraphTraversal {
            schema_version: QUERY_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
            root: request.root.clone(),
            direction: request.direction,
            nodes,
            relations,
            truncated,
        })
    }

    /// Compares normalized content across two versions of the same world.
    ///
    /// # Errors
    ///
    /// Returns an error when worlds differ or the backing store cannot
    /// enumerate either snapshot.
    pub async fn diff(
        &self,
        from: &SnapshotId,
        to: &SnapshotId,
    ) -> Result<SnapshotDiff, QueryError> {
        if from.world != to.world {
            return Err(QueryError::DifferentWorlds);
        }
        let before_entities = by_entity_id(self.store.entities(from).await?);
        let after_entities = by_entity_id(self.store.entities(to).await?);
        let before_relations = by_relation_id(self.store.snapshot_relations(from).await?);
        let after_relations = by_relation_id(self.store.snapshot_relations(to).await?);
        let (added_entities, removed_entities, changed_entities) =
            diff_entities(&before_entities, &after_entities);
        let (added_relations, removed_relations, changed_relations) =
            diff_relations(&before_relations, &after_relations);
        Ok(SnapshotDiff {
            schema_version: QUERY_SCHEMA_VERSION,
            from: from.clone(),
            to: to.clone(),
            added_entities,
            removed_entities,
            changed_entities,
            added_relations,
            removed_relations,
            changed_relations,
        })
    }
}

fn connected_entity<'a>(
    observation: &'a RelationObservation,
    entity: &EntityId,
    direction: RelationDirection,
) -> Option<&'a EntityId> {
    match direction {
        RelationDirection::Incoming | RelationDirection::Both
            if observation.relation.to == *entity =>
        {
            Some(&observation.relation.from)
        }
        RelationDirection::Outgoing | RelationDirection::Both
            if observation.relation.from == *entity =>
        {
            Some(&observation.relation.to)
        }
        _ => None,
    }
}

fn by_entity_id(observations: Vec<EntityObservation>) -> BTreeMap<EntityId, EntityObservation> {
    observations
        .into_iter()
        .map(|observation| (observation.entity.id.clone(), observation))
        .collect()
}

fn by_relation_id(
    observations: Vec<RelationObservation>,
) -> BTreeMap<RelationId, RelationObservation> {
    observations
        .into_iter()
        .map(|observation| (observation.relation.id.clone(), observation))
        .collect()
}

fn diff_entities(
    before: &BTreeMap<EntityId, EntityObservation>,
    after: &BTreeMap<EntityId, EntityObservation>,
) -> (
    Vec<EntityObservation>,
    Vec<EntityObservation>,
    Vec<EntityChange>,
) {
    let added = after
        .iter()
        .filter(|(id, _)| !before.contains_key(*id))
        .map(|(_, observation)| observation.clone())
        .collect();
    let removed = before
        .iter()
        .filter(|(id, _)| !after.contains_key(*id))
        .map(|(_, observation)| observation.clone())
        .collect();
    let changed = before
        .iter()
        .filter_map(|(id, before)| {
            let after = after.get(id)?;
            (!same_entity_content(before, after)).then(|| EntityChange {
                id: id.clone(),
                before: before.clone(),
                after: after.clone(),
            })
        })
        .collect();
    (added, removed, changed)
}

fn diff_relations(
    before: &BTreeMap<RelationId, RelationObservation>,
    after: &BTreeMap<RelationId, RelationObservation>,
) -> (
    Vec<RelationObservation>,
    Vec<RelationObservation>,
    Vec<RelationChange>,
) {
    let added = after
        .iter()
        .filter(|(id, _)| !before.contains_key(*id))
        .map(|(_, observation)| observation.clone())
        .collect();
    let removed = before
        .iter()
        .filter(|(id, _)| !after.contains_key(*id))
        .map(|(_, observation)| observation.clone())
        .collect();
    let changed = before
        .iter()
        .filter_map(|(id, before)| {
            let after = after.get(id)?;
            (!same_relation_content(before, after)).then(|| RelationChange {
                id: id.clone(),
                before: before.clone(),
                after: after.clone(),
            })
        })
        .collect();
    (added, removed, changed)
}

fn same_entity_content(left: &EntityObservation, right: &EntityObservation) -> bool {
    left.entity == right.entity
        && left.attributes == right.attributes
        && left.measurements == right.measurements
        && left.scores == right.scores
}

fn same_relation_content(left: &RelationObservation, right: &RelationObservation) -> bool {
    left.relation == right.relation
        && left.weight.to_bits() == right.weight.to_bits()
        && left.attributes == right.attributes
}

#[cfg(test)]
mod tests {
    use detamu_core::{Entity, ModelId, ObservationBatch, Relation, SnapshotVersion, WorldId};
    use detamu_store::{DetamuStore, InMemoryStore};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn filters_entities_by_domain_fields_and_attributes() {
        let (store, first, _) = fixture_store().await;
        let query = SnapshotQuery::new(store);
        let mut attributes = Attributes::new();
        attributes.insert("file_path".to_owned(), json!("src/lib.rs"));

        let matches = query
            .find_entities(
                &first,
                &EntityFilter {
                    model: Some("code".to_owned()),
                    kind: Some("function".to_owned()),
                    label_contains: Some("alpha".to_owned()),
                    attributes,
                    limit: None,
                },
            )
            .await
            .expect("find entities");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].entity.id.as_str(), "a");
    }

    #[tokio::test]
    async fn traversal_is_bounded_and_cycle_safe() {
        let (store, first, _) = fixture_store().await;
        let query = SnapshotQuery::new(store);

        let graph = query
            .traverse(
                &first,
                &GraphRequest {
                    root: EntityId::new("a"),
                    direction: RelationDirection::Outgoing,
                    max_depth: 10,
                    max_nodes: 10,
                    relation_kinds: BTreeSet::from(["calls".to_owned()]),
                },
            )
            .await
            .expect("traverse graph");

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.relations.len(), 3);
        assert!(!graph.truncated);
        assert_eq!(
            graph
                .nodes
                .iter()
                .map(|node| node.depth)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[tokio::test]
    async fn diff_ignores_snapshot_identity_and_reports_content_changes() {
        let (store, first, second) = fixture_store().await;
        let query = SnapshotQuery::new(store);

        let diff = query.diff(&first, &second).await.expect("diff snapshots");

        assert_eq!(diff.added_entities.len(), 1);
        assert_eq!(diff.added_entities[0].entity.id.as_str(), "d");
        assert_eq!(diff.removed_entities.len(), 1);
        assert_eq!(diff.removed_entities[0].entity.id.as_str(), "c");
        assert_eq!(diff.changed_entities.len(), 1);
        assert_eq!(diff.changed_entities[0].id.as_str(), "b");
        assert_eq!(diff.added_relations.len(), 1);
        assert_eq!(diff.removed_relations.len(), 2);
        assert!(diff.changed_relations.is_empty());
    }

    async fn fixture_store() -> (Arc<InMemoryStore>, SnapshotId, SnapshotId) {
        let store = Arc::new(InMemoryStore::default());
        let world = WorldId::new("code.repository:fixture");
        let first = SnapshotId::new(world.clone(), SnapshotVersion::new("v1"));
        let second = SnapshotId::new(world, SnapshotVersion::new("v2"));
        let mut first_batch = ObservationBatch::empty(first.clone());
        first_batch.entities = vec![
            entity(&first, "a", "alpha"),
            entity(&first, "b", "beta"),
            entity(&first, "c", "gamma"),
        ];
        first_batch.relations = vec![
            relation(&first, "a", "b"),
            relation(&first, "b", "c"),
            relation(&first, "c", "a"),
        ];
        store.ingest(first_batch).await.expect("ingest first");
        let mut second_batch = ObservationBatch::empty(second.clone());
        second_batch.entities = vec![
            entity(&second, "a", "alpha"),
            entity(&second, "b", "beta changed"),
            entity(&second, "d", "delta"),
        ];
        second_batch.relations = vec![relation(&second, "a", "b"), relation(&second, "a", "d")];
        store.ingest(second_batch).await.expect("ingest second");
        (store, first, second)
    }

    fn entity(snapshot: &SnapshotId, id: &str, label: &str) -> EntityObservation {
        let mut attributes = Attributes::new();
        attributes.insert("file_path".to_owned(), json!("src/lib.rs"));
        EntityObservation {
            snapshot: snapshot.clone(),
            entity: Entity {
                id: EntityId::new(id),
                model: ModelId::new("code"),
                kind: "function".to_owned(),
                label: label.to_owned(),
            },
            attributes,
            measurements: Vec::new(),
            scores: Vec::new(),
        }
    }

    fn relation(snapshot: &SnapshotId, from: &str, to: &str) -> RelationObservation {
        RelationObservation {
            snapshot: snapshot.clone(),
            relation: Relation {
                id: RelationId::new(format!("{from}:calls:{to}")),
                model: ModelId::new("code"),
                kind: "calls".to_owned(),
                from: EntityId::new(from),
                to: EntityId::new(to),
            },
            weight: 1.0,
            attributes: Attributes::new(),
        }
    }
}
