//! Persistence contract for Detamu.
//!
//! `SurrealDB` is the intended production backend. The in-memory store is both a
//! useful embedding option and the behavioral reference for backend tests.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use detamu_core::{
    DependencyObservation, ObservationBatch, RevisionId, SymbolId, SymbolObservation,
};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyDirection {
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
    async fn ingest(&self, batch: ObservationBatch) -> Result<(), StoreError>;

    async fn symbol(
        &self,
        revision: &RevisionId,
        symbol: &SymbolId,
    ) -> Result<Option<SymbolObservation>, StoreError>;

    async fn dependencies(
        &self,
        revision: &RevisionId,
        symbol: &SymbolId,
        direction: DependencyDirection,
    ) -> Result<Vec<DependencyObservation>, StoreError>;
}

#[derive(Debug, Default)]
struct MemoryState {
    symbols: HashMap<(RevisionId, SymbolId), SymbolObservation>,
    dependencies: Vec<DependencyObservation>,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryStore {
    state: Arc<RwLock<MemoryState>>,
}

#[async_trait]
impl DetamuStore for InMemoryStore {
    async fn ingest(&self, batch: ObservationBatch) -> Result<(), StoreError> {
        if batch
            .symbols
            .iter()
            .any(|observation| observation.revision != batch.revision)
            || batch
                .dependencies
                .iter()
                .any(|observation| observation.revision != batch.revision)
        {
            return Err(StoreError::InconsistentBatch(
                "an observation belongs to a different revision".to_owned(),
            ));
        }

        let mut state = self.state.write().await;
        for observation in batch.symbols {
            let key = (batch.revision.clone(), observation.symbol.id.clone());
            state.symbols.insert(key, observation);
        }

        state
            .dependencies
            .retain(|dependency| dependency.revision != batch.revision);
        state.dependencies.extend(batch.dependencies);
        Ok(())
    }

    async fn symbol(
        &self,
        revision: &RevisionId,
        symbol: &SymbolId,
    ) -> Result<Option<SymbolObservation>, StoreError> {
        Ok(self
            .state
            .read()
            .await
            .symbols
            .get(&(revision.clone(), symbol.clone()))
            .cloned())
    }

    async fn dependencies(
        &self,
        revision: &RevisionId,
        symbol: &SymbolId,
        direction: DependencyDirection,
    ) -> Result<Vec<DependencyObservation>, StoreError> {
        let dependencies = self
            .state
            .read()
            .await
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.revision == *revision
                    && match direction {
                        DependencyDirection::Incoming => dependency.to == *symbol,
                        DependencyDirection::Outgoing => dependency.from == *symbol,
                        DependencyDirection::Both => {
                            dependency.from == *symbol || dependency.to == *symbol
                        }
                    }
            })
            .cloned()
            .collect();
        Ok(dependencies)
    }
}

#[cfg(test)]
mod tests {
    use detamu_core::{
        AnalysisCoverage, AvecWeights, CodeSymbol, GitOid, LanguageId, NodeKind, NodeMetrics,
        RepositoryId,
    };

    use super::*;

    #[tokio::test]
    async fn stores_symbols_by_exact_revision() {
        let store = InMemoryStore::default();
        let revision = RevisionId::new(RepositoryId::new("repo"), GitOid::new("abc123"));
        let symbol_id = SymbolId::new("rust:detamu::run");
        let metrics = NodeMetrics::default();
        let observation = SymbolObservation {
            revision: revision.clone(),
            symbol: CodeSymbol {
                id: symbol_id.clone(),
                language: LanguageId::new("rust"),
                qualified_name: "detamu::run".to_owned(),
                kind: NodeKind::Function,
            },
            file_path: "src/lib.rs".to_owned(),
            line_start: 1,
            line_end: 4,
            signature: None,
            metrics,
            avec: AvecWeights::default().calculate(&metrics),
        };
        let mut batch = ObservationBatch::empty(revision.clone());
        batch.coverage = AnalysisCoverage::Complete;
        batch.symbols.push(observation.clone());

        store.ingest(batch).await.expect("batch should ingest");

        assert_eq!(
            store
                .symbol(&revision, &symbol_id)
                .await
                .expect("lookup should succeed"),
            Some(observation)
        );
    }
}
