//! Extension contracts for language support and analysis engines.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use detamu_core::{LanguageId, ObservationBatch, RevisionId};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnalyzerCapability {
    Symbols,
    Dependencies,
    Complexity,
    GitHistory,
    TestCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyzerDescriptor {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<AnalyzerCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisInput {
    pub repository_path: PathBuf,
    pub revision: RevisionId,
    pub changed_files: Option<Vec<PathBuf>>,
}

#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("analyzer is unavailable: {0}")]
    Unavailable(String),
    #[error("analysis failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait Analyzer: Send + Sync {
    fn descriptor(&self) -> AnalyzerDescriptor;

    fn supports(&self, language: &LanguageId) -> bool;

    async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError>;
}

pub trait LanguagePack: Send + Sync {
    fn language(&self) -> LanguageId;

    fn extensions(&self) -> &[&str];

    fn analyzers(&self) -> Vec<Arc<dyn Analyzer>>;
}
