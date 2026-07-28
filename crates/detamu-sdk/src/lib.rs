//! Embeddable Detamu facade.

use std::sync::Arc;

use detamu_core::{AnalysisCoverage, ObservationBatch, RevisionId};
use detamu_language::{AnalysisInput, Analyzer, AnalyzerError};
use detamu_store::{DetamuStore, StoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetamuError {
    #[error(transparent)]
    Analyzer(#[from] AnalyzerError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("analyzer returned observations for a different revision")]
    RevisionMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub revision: RevisionId,
    pub analyzers_run: usize,
    pub symbols: usize,
    pub dependencies: usize,
    pub coverage: AnalysisCoverage,
}

pub struct Detamu {
    store: Arc<dyn DetamuStore>,
    analyzers: Vec<Arc<dyn Analyzer>>,
}

impl Detamu {
    pub fn builder(store: Arc<dyn DetamuStore>) -> DetamuBuilder {
        DetamuBuilder {
            store,
            analyzers: Vec::new(),
        }
    }

    /// Runs every registered analyzer and persists their combined observations.
    ///
    /// # Errors
    ///
    /// Returns an error when an analyzer fails, emits a mismatched revision, or
    /// the configured store cannot commit the observation batch.
    pub async fn index(&self, input: AnalysisInput) -> Result<IndexReport, DetamuError> {
        let mut combined: Option<ObservationBatch> = None;

        for analyzer in &self.analyzers {
            let observations = analyzer.analyze(&input).await?;
            if let Some(batch) = &mut combined {
                batch
                    .merge(observations)
                    .map_err(|_| DetamuError::RevisionMismatch)?;
            } else if observations.revision == input.revision {
                combined = Some(observations);
            } else {
                return Err(DetamuError::RevisionMismatch);
            }
        }

        let combined = combined.unwrap_or_else(|| ObservationBatch::empty(input.revision));

        let report = IndexReport {
            revision: combined.revision.clone(),
            analyzers_run: self.analyzers.len(),
            symbols: combined.symbols.len(),
            dependencies: combined.dependencies.len(),
            coverage: combined.coverage,
        };
        self.store.ingest(combined).await?;
        Ok(report)
    }

    /// Persists an observation batch supplied by an external indexing pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured store rejects or cannot persist the
    /// batch.
    pub async fn ingest(&self, batch: ObservationBatch) -> Result<(), DetamuError> {
        self.store.ingest(batch).await?;
        Ok(())
    }

    pub fn store(&self) -> &Arc<dyn DetamuStore> {
        &self.store
    }
}

pub struct DetamuBuilder {
    store: Arc<dyn DetamuStore>,
    analyzers: Vec<Arc<dyn Analyzer>>,
}

impl DetamuBuilder {
    #[must_use]
    pub fn analyzer(mut self, analyzer: Arc<dyn Analyzer>) -> Self {
        self.analyzers.push(analyzer);
        self
    }

    pub fn build(self) -> Detamu {
        Detamu {
            store: self.store,
            analyzers: self.analyzers,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;
    use detamu_core::{GitOid, LanguageId, RepositoryId};
    use detamu_language::{AnalyzerCapability, AnalyzerDescriptor};
    use detamu_store::InMemoryStore;

    use super::*;

    struct EmptyAnalyzer;

    #[async_trait]
    impl Analyzer for EmptyAnalyzer {
        fn descriptor(&self) -> AnalyzerDescriptor {
            AnalyzerDescriptor {
                name: "empty".to_owned(),
                version: "1".to_owned(),
                capabilities: vec![AnalyzerCapability::Symbols],
            }
        }

        fn supports(&self, _language: &LanguageId) -> bool {
            true
        }

        async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
            let mut batch = ObservationBatch::empty(input.revision.clone());
            batch.coverage = AnalysisCoverage::Complete;
            Ok(batch)
        }
    }

    #[tokio::test]
    async fn embedded_sdk_runs_registered_analyzers() {
        let store = Arc::new(InMemoryStore::default());
        let detamu = Detamu::builder(store)
            .analyzer(Arc::new(EmptyAnalyzer))
            .build();
        let revision = RevisionId::new(RepositoryId::new("repo"), GitOid::new("abc123"));

        let report = detamu
            .index(AnalysisInput {
                repository_path: PathBuf::from("."),
                revision: revision.clone(),
                changed_files: None,
            })
            .await
            .expect("index should succeed");

        assert_eq!(report.revision, revision);
        assert_eq!(report.analyzers_run, 1);
        assert_eq!(report.coverage, AnalysisCoverage::Complete);
    }
}
