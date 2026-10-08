//! Shared execution layer for Tree-sitter-backed Detamu language packs.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use detamu_core::{
    AnalysisCoverage, AnalysisDiagnostic, DiagnosticSeverity, ModelId, ObservationAccumulator,
    ObservationBatch, ObserverProvenance,
};
use detamu_model::{
    ARTIFACT_READ_BUDGET_BYTES, AnalysisInput, AnalyzerCapability, AnalyzerDescriptor,
    AnalyzerError, AnalyzerExecution, Artifact, ArtifactReader, ModelAnalyzer,
    artifact_read_groups,
};
use detamu_model_code::{CODE_MODEL_ID, GitOid, LanguageId, RepositoryId, RevisionId};
use tree_sitter::{Language, Node, Parser};

pub trait TreeSitterSpec: Send + Sync + 'static {
    fn language(&self) -> LanguageId;

    fn extensions(&self) -> &[&str];

    fn grammar(&self) -> Language;

    fn observer(&self) -> &'static str;

    fn version(&self) -> &'static str;

    fn configuration_digest(&self) -> &'static str;

    fn capabilities(&self) -> Vec<AnalyzerCapability>;

    fn observe_tree(
        &self,
        revision: &RevisionId,
        artifact: &Artifact,
        source: &[u8],
        root: Node<'_>,
        batch: &mut ObservationBatch,
    );
}

pub struct TreeSitterAnalyzer<S> {
    artifacts: Arc<dyn ArtifactReader>,
    spec: Arc<S>,
}

impl<S> TreeSitterAnalyzer<S> {
    pub fn new(artifacts: Arc<dyn ArtifactReader>, spec: S) -> Self {
        Self {
            artifacts,
            spec: Arc::new(spec),
        }
    }
}

#[async_trait]
impl<S: TreeSitterSpec> ModelAnalyzer for TreeSitterAnalyzer<S> {
    fn descriptor(&self) -> AnalyzerDescriptor {
        AnalyzerDescriptor {
            name: self.spec.observer().to_owned(),
            version: self.spec.version().to_owned(),
            model: ModelId::new(CODE_MODEL_ID),
            capabilities: self.spec.capabilities(),
            execution: AnalyzerExecution::Required,
        }
    }

    async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
        let source = input
            .sources
            .iter()
            .find(|source| self.artifacts.supports(source))
            .ok_or_else(|| AnalyzerError::Unavailable("artifact source is missing".to_owned()))?;
        let artifacts = self
            .artifacts
            .artifacts(source)
            .await
            .map_err(|error| AnalyzerError::Failed(error.to_string()))?
            .into_iter()
            .filter(|artifact| supports_artifact(self.spec.as_ref(), artifact))
            .collect::<Vec<_>>();
        let revision = revision(input)?;
        let mut accumulated = ObservationAccumulator::new(revision.snapshot());
        accumulated.set_coverage(AnalysisCoverage::Partial);
        accumulated.push_provenance(ObserverProvenance {
            observer: self.spec.observer().to_owned(),
            version: self.spec.version().to_owned(),
            configuration_digest: Some(self.spec.configuration_digest().to_owned()),
            source: None,
        });
        for group in artifact_read_groups(&artifacts, ARTIFACT_READ_BUDGET_BYTES) {
            let contents = self
                .artifacts
                .read_many(source, &artifacts[group])
                .await
                .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
            let spec = Arc::clone(&self.spec);
            let revision = revision.clone();
            let observations = tokio::task::spawn_blocking(move || {
                observe_contents(spec.as_ref(), &revision, contents)
            })
            .await
            .map_err(|error| {
                AnalyzerError::Failed(format!("Tree-sitter task failed: {error}"))
            })??;
            accumulated.merge(observations).map_err(|_| {
                AnalyzerError::Failed(format!(
                    "{} emitted conflicting observations",
                    self.spec.observer()
                ))
            })?;
        }
        Ok(accumulated.finish())
    }
}

fn observe_contents<S: TreeSitterSpec>(
    spec: &S,
    revision: &RevisionId,
    contents: Vec<detamu_model::ArtifactContent>,
) -> Result<ObservationBatch, AnalyzerError> {
    let mut parser = Parser::new();
    parser.set_language(&spec.grammar()).map_err(|error| {
        AnalyzerError::Failed(format!(
            "load {} grammar: {error}",
            spec.language().as_str()
        ))
    })?;
    let mut accumulated = ObservationAccumulator::new(revision.snapshot());
    for content in contents {
        let mut observations = ObservationBatch::empty(revision.snapshot());
        let Some(tree) = parser.parse(&content.bytes, None) else {
            observations.diagnostics.push(diagnostic(
                spec.observer(),
                &content.artifact.path,
                "Tree-sitter did not produce a syntax tree",
            ));
            accumulated.merge(observations).map_err(|_| {
                AnalyzerError::Failed(format!(
                    "{} emitted conflicting observations",
                    spec.observer()
                ))
            })?;
            continue;
        };
        if tree.root_node().has_error() {
            observations.diagnostics.push(diagnostic(
                spec.observer(),
                &content.artifact.path,
                "source contains syntax errors; observations may be incomplete",
            ));
        }
        spec.observe_tree(
            revision,
            &content.artifact,
            &content.bytes,
            tree.root_node(),
            &mut observations,
        );
        accumulated.merge(observations).map_err(|_| {
            AnalyzerError::Failed(format!(
                "{} emitted conflicting observations",
                spec.observer()
            ))
        })?;
    }
    Ok(accumulated.finish())
}

fn supports_artifact(spec: &dyn TreeSitterSpec, artifact: &Artifact) -> bool {
    if artifact
        .attributes
        .get("language")
        .and_then(serde_json::Value::as_str)
        == Some(spec.language().as_str())
    {
        return true;
    }
    Path::new(&artifact.path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| spec.extensions().contains(&extension))
}

fn revision(input: &AnalysisInput) -> Result<RevisionId, AnalyzerError> {
    let repository = input
        .snapshot
        .world
        .as_str()
        .strip_prefix("code.repository:")
        .ok_or_else(|| AnalyzerError::Failed("snapshot is not a code repository".to_owned()))?;
    Ok(RevisionId::new(
        RepositoryId::new(repository),
        GitOid::new(input.snapshot.version.as_str()),
    ))
}

fn diagnostic(observer: &str, path: &str, message: &str) -> AnalysisDiagnostic {
    AnalysisDiagnostic {
        severity: DiagnosticSeverity::Warning,
        observer: observer.to_owned(),
        message: message.to_owned(),
        scope: Some(path.to_owned()),
    }
}
