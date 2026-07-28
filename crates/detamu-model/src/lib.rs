//! Extension contracts for world models, observers, and scoring models.

use std::sync::Arc;

use async_trait::async_trait;
use detamu_core::{Attributes, EntityId, ModelId, ObservationBatch, SnapshotId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: ModelId,
    pub version: u32,
    pub entity_kinds: Vec<String>,
    pub relation_kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceReference {
    pub kind: String,
    pub locator: String,
    pub cursor: Option<String>,
    pub attributes: Attributes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescriptor {
    pub name: String,
    pub version: String,
    pub model: ModelId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRequest {
    pub locator: String,
    /// A source-native immutable version, or the source's current version when absent.
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceResolution {
    pub input: AnalysisInput,
    pub metadata: Attributes,
}

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("source is unavailable: {0}")]
    Unavailable(String),
    #[error("source resolution failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait WorldSource: Send + Sync {
    fn descriptor(&self) -> SourceDescriptor;

    async fn resolve(&self, request: &SourceRequest) -> Result<SourceResolution, SourceError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    pub path: String,
    pub content_id: String,
    pub media_type: Option<String>,
    pub attributes: Attributes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactContent {
    pub artifact: Artifact,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact source is unavailable: {0}")]
    Unavailable(String),
    #[error("artifact read failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait ArtifactReader: Send + Sync {
    fn supports(&self, source: &SourceReference) -> bool;

    async fn artifacts(&self, source: &SourceReference) -> Result<Vec<Artifact>, ArtifactError>;

    async fn read_many(
        &self,
        source: &SourceReference,
        artifacts: &[Artifact],
    ) -> Result<Vec<ArtifactContent>, ArtifactError>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisInput {
    pub snapshot: SnapshotId,
    pub sources: Vec<SourceReference>,
    pub changed_entities: Option<Vec<EntityId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzerDescriptor {
    pub name: String,
    pub version: String,
    pub model: ModelId,
    pub capabilities: Vec<AnalyzerCapability>,
    pub execution: AnalyzerExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerCapability {
    Symbols,
    Metrics,
    Hierarchy,
    Imports,
    References,
    Calls,
    Types,
    Diagnostics,
    Other(String),
}

impl AnalyzerCapability {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Symbols => "symbols",
            Self::Metrics => "metrics",
            Self::Hierarchy => "hierarchy",
            Self::Imports => "imports",
            Self::References => "references",
            Self::Calls => "calls",
            Self::Types => "types",
            Self::Diagnostics => "diagnostics",
            Self::Other(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerExecution {
    Required,
    Optional,
}

#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("analyzer is unavailable: {0}")]
    Unavailable(String),
    #[error("analysis failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait ModelAnalyzer: Send + Sync {
    fn descriptor(&self) -> AnalyzerDescriptor;

    async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoringModelDescriptor {
    pub id: detamu_core::ScoreModelId,
    pub version: u32,
    pub model: ModelId,
    pub dimensions: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ScoringError {
    #[error("scoring model cannot score this batch: {0}")]
    Unsupported(String),
    #[error("scoring failed: {0}")]
    Failed(String),
}

pub trait ScoringModel: Send + Sync {
    fn descriptor(&self) -> ScoringModelDescriptor;

    /// Adds derived scores to observations without changing source evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when required measurements are missing or invalid.
    fn score(&self, batch: &mut ObservationBatch) -> Result<(), ScoringError>;
}

pub trait WorldModelPack: Send + Sync {
    fn descriptor(&self) -> ModelDescriptor;

    fn analyzers(&self) -> Vec<Arc<dyn ModelAnalyzer>>;

    fn scoring_models(&self) -> Vec<Arc<dyn ScoringModel>>;
}
