//! Native `SurrealDB` storage for Detamu's world-model substrate.

mod schema;

use std::path::Path;

use async_trait::async_trait;
use detamu_core::{
    AnalysisCoverage, CommitMode, EntityId, EntityObservation, ObservationBatch,
    RelationObservation, SnapshotId,
};
use detamu_store::{DetamuStore, RelationDirection, StoreError, validate_batch};
use serde_json::{Value, json};
use surrealdb::{
    Connection, Surreal,
    engine::local::{Db, Mem, SurrealKv},
};
use thiserror::Error;

const DEFAULT_WRITE_BATCH_SIZE: usize = 1_000;

#[derive(Debug, Error)]
pub enum SurrealStoreError {
    #[error(transparent)]
    Database(#[from] surrealdb::Error),
    #[error("observation serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub struct SurrealStore<C: Connection> {
    db: Surreal<C>,
    write_batch_size: usize,
}

impl<C: Connection> Clone for SurrealStore<C> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            write_batch_size: self.write_batch_size,
        }
    }
}

impl<C: Connection> SurrealStore<C> {
    pub fn new(db: Surreal<C>) -> Self {
        Self {
            db,
            write_batch_size: DEFAULT_WRITE_BATCH_SIZE,
        }
    }

    #[must_use]
    pub fn with_write_batch_size(mut self, size: usize) -> Self {
        self.write_batch_size = size.max(1);
        self
    }

    pub fn database(&self) -> &Surreal<C> {
        &self.db
    }

    /// Defines the generic Detamu schema idempotently.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema cannot be applied.
    pub async fn ensure_schema(&self) -> Result<(), SurrealStoreError> {
        self.db.query(schema::SCHEMA).await?.check()?;
        Ok(())
    }

    async fn ingest_transaction(&self, batch: ObservationBatch) -> Result<(), SurrealStoreError> {
        let world_id = batch.snapshot.world.as_str().to_owned();
        let snapshot_version = batch.snapshot.version.as_str().to_owned();
        let entities = batch
            .entities
            .iter()
            .map(entity_row)
            .collect::<Result<Vec<_>, _>>()?;
        let relations = batch
            .relations
            .iter()
            .map(relation_row)
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = snapshot_row(&batch)?;
        let transaction = self.db.clone().begin().await?;
        let result = async {
            transaction.query("DELETE detamu_relation_observation WHERE world_id = $world_id AND snapshot_version = $snapshot_version").bind(("world_id", world_id.clone())).bind(("snapshot_version", snapshot_version.clone())).await?.check()?;
            transaction.query("DELETE detamu_entity_observation WHERE world_id = $world_id AND snapshot_version = $snapshot_version").bind(("world_id", world_id.clone())).bind(("snapshot_version", snapshot_version.clone())).await?.check()?;
            transaction.query("DELETE detamu_snapshot WHERE world_id = $world_id AND snapshot_version = $snapshot_version").bind(("world_id", world_id)).bind(("snapshot_version", snapshot_version)).await?.check()?;
            for rows in entities.chunks(self.write_batch_size) {
                transaction.query("INSERT INTO detamu_entity_observation $rows").bind(("rows", rows.to_vec())).await?.check()?;
            }
            for rows in relations.chunks(self.write_batch_size) {
                transaction.query("INSERT INTO detamu_relation_observation $rows").bind(("rows", rows.to_vec())).await?.check()?;
            }
            transaction.query("INSERT INTO detamu_snapshot $row").bind(("row", snapshot)).await?.check()?;
            Ok::<(), surrealdb::Error>(())
        }.await;
        match result {
            Ok(()) => {
                transaction.commit().await?;
                Ok(())
            }
            Err(error) => {
                let _ = transaction.cancel().await;
                Err(error.into())
            }
        }
    }
}

impl SurrealStore<Db> {
    /// Creates an isolated in-memory store with the Detamu schema.
    ///
    /// # Errors
    ///
    /// Returns an error when the embedded database cannot start or initialize.
    pub async fn memory(namespace: &str, database: &str) -> Result<Self, SurrealStoreError> {
        let db = Surreal::new::<Mem>(()).await?;
        db.use_ns(namespace).use_db(database).await?;
        let store = Self::new(db);
        store.ensure_schema().await?;
        Ok(store)
    }

    /// Opens a persistent embedded `SurrealKV` store with the Detamu schema.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot open or initialize.
    pub async fn surrealkv(
        path: impl AsRef<Path>,
        namespace: &str,
        database: &str,
    ) -> Result<Self, SurrealStoreError> {
        let path = path.as_ref().to_string_lossy().into_owned();
        let db = Surreal::new::<SurrealKv>(path).await?;
        db.use_ns(namespace).use_db(database).await?;
        let store = Self::new(db);
        store.ensure_schema().await?;
        Ok(store)
    }
}

#[async_trait]
impl<C> DetamuStore for SurrealStore<C>
where
    C: Connection + Send + Sync + 'static,
{
    async fn ingest(&self, batch: ObservationBatch) -> Result<(), StoreError> {
        validate_batch(&batch)?;
        self.ingest_transaction(batch)
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    async fn entity(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
    ) -> Result<Option<EntityObservation>, StoreError> {
        let mut response = self.db.query("SELECT payload FROM detamu_entity_observation WHERE world_id = $world_id AND snapshot_version = $snapshot_version AND entity_id = $entity_id LIMIT 1")
            .bind(("world_id", snapshot.world.as_str().to_owned())).bind(("snapshot_version", snapshot.version.as_str().to_owned())).bind(("entity_id", entity.as_str().to_owned())).await.map_err(backend)?;
        let rows: Vec<Value> = response.take(0).map_err(backend)?;
        rows.into_iter()
            .next()
            .and_then(|row| row.get("payload").cloned())
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    async fn relations(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
        direction: RelationDirection,
    ) -> Result<Vec<RelationObservation>, StoreError> {
        let endpoint_filter = match direction {
            RelationDirection::Incoming => "to_entity_id = $entity_id",
            RelationDirection::Outgoing => "from_entity_id = $entity_id",
            RelationDirection::Both => "(from_entity_id = $entity_id OR to_entity_id = $entity_id)",
        };
        let query = format!(
            "SELECT payload FROM detamu_relation_observation WHERE world_id = $world_id AND snapshot_version = $snapshot_version AND {endpoint_filter}"
        );
        let mut response = self
            .db
            .query(query)
            .bind(("world_id", snapshot.world.as_str().to_owned()))
            .bind(("snapshot_version", snapshot.version.as_str().to_owned()))
            .bind(("entity_id", entity.as_str().to_owned()))
            .await
            .map_err(backend)?;
        let rows: Vec<Value> = response.take(0).map_err(backend)?;
        rows.into_iter()
            .filter_map(|row| row.get("payload").cloned())
            .map(|payload| {
                serde_json::from_value(payload)
                    .map_err(|error| StoreError::Backend(error.to_string()))
            })
            .collect()
    }
}

#[allow(clippy::needless_pass_by_value)]
fn backend(error: surrealdb::Error) -> StoreError {
    StoreError::Backend(error.to_string())
}

fn entity_row(observation: &EntityObservation) -> Result<Value, serde_json::Error> {
    Ok(json!({
        "world_id": observation.snapshot.world.as_str(), "snapshot_version": observation.snapshot.version.as_str(),
        "entity_id": observation.entity.id.as_str(), "model_id": observation.entity.model.as_str(),
        "entity_kind": observation.entity.kind, "label": observation.entity.label,
        "payload": serde_json::to_value(observation)?,
    }))
}

fn relation_row(observation: &RelationObservation) -> Result<Value, serde_json::Error> {
    Ok(json!({
        "world_id": observation.snapshot.world.as_str(), "snapshot_version": observation.snapshot.version.as_str(),
        "relation_id": observation.relation.id.as_str(), "model_id": observation.relation.model.as_str(),
        "relation_kind": observation.relation.kind, "from_entity_id": observation.relation.from.as_str(),
        "to_entity_id": observation.relation.to.as_str(), "weight": observation.weight,
        "payload": serde_json::to_value(observation)?,
    }))
}

fn snapshot_row(batch: &ObservationBatch) -> Result<Value, serde_json::Error> {
    Ok(json!({
        "world_id": batch.snapshot.world.as_str(), "snapshot_version": batch.snapshot.version.as_str(),
        "commit_mode": commit_mode(batch.commit_mode), "coverage": coverage(batch.coverage),
        "provenance": serde_json::to_value(&batch.provenance)?, "diagnostics": serde_json::to_value(&batch.diagnostics)?,
        "entity_count": batch.entities.len(), "relation_count": batch.relations.len(),
    }))
}

fn commit_mode(mode: CommitMode) -> &'static str {
    match mode {
        CommitMode::ReplaceSnapshot => "replace_snapshot",
    }
}
fn coverage(value: AnalysisCoverage) -> &'static str {
    match value {
        AnalysisCoverage::Complete => "complete",
        AnalysisCoverage::Partial => "partial",
        AnalysisCoverage::Unavailable => "unavailable",
    }
}
