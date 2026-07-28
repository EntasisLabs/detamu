use serde::{Deserialize, Serialize};

use crate::{AvecScores, LanguageId, RevisionId, SymbolId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalyzerProvenance {
    pub analyzer: String,
    pub version: String,
    pub configuration_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Module,
    Namespace,
    Type,
    Trait,
    Interface,
    Function,
    Method,
    Field,
    Constant,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub id: SymbolId,
    pub language: LanguageId,
    pub qualified_name: String,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub lines_of_code: u32,
    pub cyclomatic_complexity: u32,
    pub parameters: u32,
    pub incoming_edges: u32,
    pub outgoing_edges: u32,
    pub git_total_commits: u32,
    pub git_contributors: u32,
    pub git_average_days_between_changes: f64,
    /// Normalized to `0.0..=1.0`.
    pub test_line_coverage: f64,
    /// Normalized to `0.0..=1.0`.
    pub test_branch_coverage: f64,
}

impl NodeMetrics {
    pub fn total_degree(self) -> u32 {
        self.incoming_edges.saturating_add(self.outgoing_edges)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolObservation {
    pub revision: RevisionId,
    pub symbol: CodeSymbol,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub metrics: NodeMetrics,
    pub avec: AvecScores,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyType {
    Calls,
    Imports,
    References,
    Implements,
    Inherits,
    Contains,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencyObservation {
    pub revision: RevisionId,
    pub from: SymbolId,
    pub to: SymbolId,
    pub relationship: DependencyType,
    pub weight: f64,
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
    pub analyzer: String,
    pub message: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationBatch {
    pub revision: RevisionId,
    pub provenance: Vec<AnalyzerProvenance>,
    pub coverage: AnalysisCoverage,
    pub symbols: Vec<SymbolObservation>,
    pub dependencies: Vec<DependencyObservation>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

impl ObservationBatch {
    pub fn empty(revision: RevisionId) -> Self {
        Self {
            revision,
            provenance: Vec::new(),
            coverage: AnalysisCoverage::Unavailable,
            symbols: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Adds observations from another analyzer to this batch.
    ///
    /// # Errors
    ///
    /// Returns [`RevisionMismatch`] when the batches describe different
    /// repository revisions.
    pub fn merge(&mut self, mut other: Self) -> Result<(), RevisionMismatch> {
        if self.revision != other.revision {
            return Err(RevisionMismatch);
        }

        self.provenance.append(&mut other.provenance);
        self.symbols.append(&mut other.symbols);
        self.dependencies.append(&mut other.dependencies);
        self.diagnostics.append(&mut other.diagnostics);
        self.coverage = merge_coverage(self.coverage, other.coverage);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionMismatch;

fn merge_coverage(left: AnalysisCoverage, right: AnalysisCoverage) -> AnalysisCoverage {
    use AnalysisCoverage::{Complete, Partial, Unavailable};

    match (left, right) {
        (Unavailable, Unavailable) => Unavailable,
        (Complete, Complete) => Complete,
        _ => Partial,
    }
}
