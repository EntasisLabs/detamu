//! Embeddable, world-model-agnostic Detamu orchestration facade.

use std::sync::Arc;

use detamu_core::{
    AnalysisCoverage, AnalysisDiagnostic, DiagnosticSeverity, ObservationBatch, SnapshotId,
};
use detamu_model::{
    AnalysisInput, AnalyzerError, AnalyzerExecution, ModelAnalyzer, ScoringError, ScoringModel,
    SourceError, SourceRequest, WorldSource,
};
use detamu_store::{DetamuStore, StoreError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetamuError {
    #[error(transparent)]
    Analyzer(#[from] AnalyzerError),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Scoring(#[from] ScoringError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("analyzer returned observations for a different snapshot")]
    SnapshotMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub snapshot: SnapshotId,
    pub analyzers_run: usize,
    pub analyzers_skipped: usize,
    pub scoring_models_run: usize,
    pub entities: usize,
    pub relations: usize,
    pub coverage: AnalysisCoverage,
}

pub struct Detamu {
    store: Arc<dyn DetamuStore>,
    analyzers: Vec<Arc<dyn ModelAnalyzer>>,
    scoring_models: Vec<Arc<dyn ScoringModel>>,
}

impl Detamu {
    pub fn builder(store: Arc<dyn DetamuStore>) -> DetamuBuilder {
        DetamuBuilder {
            store,
            analyzers: Vec::new(),
            scoring_models: Vec::new(),
        }
    }

    /// Runs every registered observer and scorer, then atomically persists the
    /// combined snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when observation, scoring, or persistence fails.
    pub async fn index(&self, input: AnalysisInput) -> Result<IndexReport, DetamuError> {
        let mut combined: Option<ObservationBatch> = None;
        let mut analyzers_run = 0;
        let mut analyzers_skipped = 0;

        for analyzer in &self.analyzers {
            let descriptor = analyzer.descriptor();
            let observations = match analyzer.analyze(&input).await {
                Ok(observations) => observations,
                Err(AnalyzerError::Unavailable(message))
                    if descriptor.execution == AnalyzerExecution::Optional =>
                {
                    analyzers_skipped += 1;
                    let batch = combined
                        .get_or_insert_with(|| ObservationBatch::empty(input.snapshot.clone()));
                    batch.coverage = AnalysisCoverage::Partial;
                    batch.diagnostics.push(AnalysisDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        observer: descriptor.name,
                        message: format!("optional analyzer unavailable: {message}"),
                        scope: None,
                    });
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            analyzers_run += 1;
            if observations.snapshot != input.snapshot {
                return Err(DetamuError::SnapshotMismatch);
            }
            if let Some(batch) = &mut combined {
                batch
                    .merge(observations)
                    .map_err(|_| DetamuError::SnapshotMismatch)?;
            } else {
                combined = Some(observations);
            }
        }

        let mut combined = combined.unwrap_or_else(|| ObservationBatch::empty(input.snapshot));
        for scoring_model in &self.scoring_models {
            scoring_model.score(&mut combined)?;
        }

        let report = IndexReport {
            snapshot: combined.snapshot.clone(),
            analyzers_run,
            analyzers_skipped,
            scoring_models_run: self.scoring_models.len(),
            entities: combined.entities.len(),
            relations: combined.relations.len(),
            coverage: combined.coverage,
        };
        self.store.ingest(combined).await?;
        Ok(report)
    }

    /// Resolves a world source to an immutable snapshot and indexes it.
    ///
    /// # Errors
    ///
    /// Returns an error when source resolution, observation, scoring, or
    /// persistence fails.
    pub async fn index_source(
        &self,
        source: &dyn WorldSource,
        request: &SourceRequest,
    ) -> Result<IndexReport, DetamuError> {
        let resolution = source.resolve(request).await?;
        self.index(resolution.input).await
    }

    /// Persists a normalized batch supplied by an external model pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured store rejects or cannot commit the batch.
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
    analyzers: Vec<Arc<dyn ModelAnalyzer>>,
    scoring_models: Vec<Arc<dyn ScoringModel>>,
}

impl DetamuBuilder {
    #[must_use]
    pub fn analyzer(mut self, analyzer: Arc<dyn ModelAnalyzer>) -> Self {
        self.analyzers.push(analyzer);
        self
    }

    #[must_use]
    pub fn analyzers(
        mut self,
        analyzers: impl IntoIterator<Item = Arc<dyn ModelAnalyzer>>,
    ) -> Self {
        self.analyzers.extend(analyzers);
        self
    }

    #[must_use]
    pub fn scoring_model(mut self, scoring_model: Arc<dyn ScoringModel>) -> Self {
        self.scoring_models.push(scoring_model);
        self
    }

    pub fn build(self) -> Detamu {
        Detamu {
            store: self.store,
            analyzers: self.analyzers,
            scoring_models: self.scoring_models,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use detamu_core::{ModelId, ScoreModelId, SnapshotVersion, WorldId};
    use detamu_model::{
        AnalyzerCapability, AnalyzerDescriptor, AnalyzerExecution, ScoringModelDescriptor,
        SourceReference,
    };
    use detamu_store::InMemoryStore;

    use super::*;

    struct EmptyAnalyzer;
    struct MissingOptionalAnalyzer;
    struct EmptyScorer;

    #[async_trait]
    impl ModelAnalyzer for EmptyAnalyzer {
        fn descriptor(&self) -> AnalyzerDescriptor {
            AnalyzerDescriptor {
                name: "empty".to_owned(),
                version: "1".to_owned(),
                model: ModelId::new("code"),
                capabilities: vec![AnalyzerCapability::Symbols],
                execution: AnalyzerExecution::Required,
            }
        }

        async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
            let mut batch = ObservationBatch::empty(input.snapshot.clone());
            batch.coverage = AnalysisCoverage::Complete;
            Ok(batch)
        }
    }

    impl ScoringModel for EmptyScorer {
        fn descriptor(&self) -> ScoringModelDescriptor {
            ScoringModelDescriptor {
                id: ScoreModelId::new("test.score"),
                version: 1,
                model: ModelId::new("test"),
                dimensions: Vec::new(),
            }
        }

        fn score(&self, _batch: &mut ObservationBatch) -> Result<(), ScoringError> {
            Ok(())
        }
    }

    #[async_trait]
    impl ModelAnalyzer for MissingOptionalAnalyzer {
        fn descriptor(&self) -> AnalyzerDescriptor {
            AnalyzerDescriptor {
                name: "missing.optional".to_owned(),
                version: "1".to_owned(),
                model: ModelId::new("code"),
                capabilities: vec![AnalyzerCapability::Metrics],
                execution: AnalyzerExecution::Optional,
            }
        }

        async fn analyze(&self, _input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
            Err(AnalyzerError::Unavailable("not installed".to_owned()))
        }
    }

    #[tokio::test]
    async fn embedded_sdk_runs_registered_model_extensions() {
        let store = Arc::new(InMemoryStore::default());
        let detamu = Detamu::builder(store)
            .analyzer(Arc::new(EmptyAnalyzer))
            .scoring_model(Arc::new(EmptyScorer))
            .build();
        let snapshot = SnapshotId::new(
            WorldId::new("code.repository:repo"),
            SnapshotVersion::new("abc123"),
        );
        let report = detamu
            .index(AnalysisInput {
                snapshot: snapshot.clone(),
                sources: Vec::<SourceReference>::new(),
                changed_entities: None,
            })
            .await
            .expect("index");
        assert_eq!(report.snapshot, snapshot);
        assert_eq!(report.analyzers_run, 1);
        assert_eq!(report.scoring_models_run, 1);
        assert_eq!(report.coverage, AnalysisCoverage::Complete);
    }

    #[tokio::test]
    async fn unavailable_optional_analyzers_do_not_abort_indexing() {
        let store = Arc::new(InMemoryStore::default());
        let detamu = Detamu::builder(store)
            .analyzer(Arc::new(EmptyAnalyzer))
            .analyzer(Arc::new(MissingOptionalAnalyzer))
            .build();
        let report = detamu
            .index(AnalysisInput {
                snapshot: SnapshotId::new(
                    WorldId::new("code.repository:repo"),
                    SnapshotVersion::new("abc123"),
                ),
                sources: Vec::new(),
                changed_entities: None,
            })
            .await
            .expect("index without optional analyzer");
        assert_eq!(report.analyzers_run, 1);
        assert_eq!(report.analyzers_skipped, 1);
        assert_eq!(report.coverage, AnalysisCoverage::Partial);
    }
}
