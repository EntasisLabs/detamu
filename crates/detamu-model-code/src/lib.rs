//! Strongly typed code world model for Detamu.
//!
//! ACC-compatible metrics and AVEC formulas live here rather than in Detamu's
//! world-model-agnostic kernel.

mod avec;

use std::collections::BTreeMap;

use detamu_core::{
    Attributes, Entity, EntityId, EntityObservation, Measurement, ModelId, Relation, RelationId,
    RelationObservation, SnapshotId, SnapshotVersion, WorldId,
};
use detamu_model::{ModelAnalyzer, ModelDescriptor, ScoringModel, WorldModelPack};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use avec::{
    AutonomyWeights, AvecCodeScorer, AvecScores, AvecWeights, FrictionWeights, LogicWeights,
    StabilityWeights,
};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }
    };
}

pub const CODE_MODEL_ID: &str = "code";

string_id!(RepositoryId);
string_id!(GitOid);
string_id!(SymbolId);
string_id!(LanguageId);

#[derive(Debug, Clone, Copy, Default)]
pub struct CodeModelPack {
    pub avec: AvecCodeScorer,
}

impl WorldModelPack for CodeModelPack {
    fn descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            id: ModelId::new(CODE_MODEL_ID),
            version: 1,
            entity_kinds: [
                "module",
                "file",
                "namespace",
                "type",
                "trait",
                "interface",
                "function",
                "method",
                "field",
                "constant",
                "unknown",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            relation_kinds: [
                "calls",
                "imports",
                "references",
                "implements",
                "inherits",
                "contains",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }

    fn analyzers(&self) -> Vec<std::sync::Arc<dyn ModelAnalyzer>> {
        Vec::new()
    }

    fn scoring_models(&self) -> Vec<std::sync::Arc<dyn ScoringModel>> {
        vec![std::sync::Arc::new(self.avec)]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RevisionId {
    pub repository: RepositoryId,
    pub commit: GitOid,
}

impl RevisionId {
    pub fn new(repository: impl Into<RepositoryId>, commit: impl Into<GitOid>) -> Self {
        Self {
            repository: repository.into(),
            commit: commit.into(),
        }
    }

    pub fn snapshot(&self) -> SnapshotId {
        SnapshotId::new(
            WorldId::new(format!("code.repository:{}", self.repository.as_str())),
            SnapshotVersion::new(self.commit.as_str()),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
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

impl NodeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Namespace => "namespace",
            Self::Type => "type",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Function => "function",
            Self::Method => "method",
            Self::Field => "field",
            Self::Constant => "constant",
            Self::Unknown => "unknown",
        }
    }
}

pub fn file_observation(
    revision: &RevisionId,
    path: &str,
    blob_oid: &str,
    mode: &str,
    size: Option<u64>,
    language: &LanguageId,
    history: Option<&FileHistory>,
) -> EntityObservation {
    let mut attributes = BTreeMap::new();
    attributes.insert("file_path".to_owned(), json!(path));
    attributes.insert("language".to_owned(), json!(language.as_str()));
    attributes.insert("git.blob_oid".to_owned(), json!(blob_oid));
    attributes.insert("git.mode".to_owned(), json!(mode));
    attributes.insert("file.size_bytes".to_owned(), json!(size));

    let mut measurements = Vec::new();
    if let Some(history) = history {
        attributes.insert("git.created_at".to_owned(), json!(history.created_at));
        attributes.insert(
            "git.last_modified_at".to_owned(),
            json!(history.last_modified_at),
        );
        attributes.insert(
            "git.recent_frequency".to_owned(),
            json!(history.recent_frequency.as_str()),
        );
        measurements.extend([
            measurement("git.total_commits", history.total_commits),
            measurement("git.contributors", history.contributors),
            measurement("git.recent_commits", history.recent_commits),
            Measurement {
                name: "git.average_days_between_changes".to_owned(),
                value: history.average_days_between_changes,
                unit: Some("days".to_owned()),
            },
        ]);
    }

    EntityObservation {
        snapshot: revision.snapshot(),
        entity: Entity {
            id: EntityId::new(format!("file:{path}")),
            model: ModelId::new(CODE_MODEL_ID),
            kind: NodeKind::File.as_str().to_owned(),
            label: path.to_owned(),
        },
        attributes,
        measurements,
        scores: Vec::new(),
    }
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
    pub test_line_coverage: f64,
    pub test_branch_coverage: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentFrequency {
    Low,
    Medium,
    High,
}

impl RecentFrequency {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub const fn from_recent_commits(commits: u32) -> Self {
        match commits {
            0..=2 => Self::Low,
            3..=9 => Self::Medium,
            _ => Self::High,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileHistory {
    pub created_at: String,
    pub last_modified_at: String,
    pub total_commits: u32,
    pub contributors: u32,
    pub average_days_between_changes: f64,
    pub recent_commits: u32,
    pub recent_frequency: RecentFrequency,
}

impl NodeMetrics {
    pub fn total_degree(self) -> u32 {
        self.incoming_edges.saturating_add(self.outgoing_edges)
    }

    pub fn measurements(self) -> Vec<Measurement> {
        vec![
            measurement("code.lines_of_code", self.lines_of_code),
            measurement("code.cyclomatic_complexity", self.cyclomatic_complexity),
            measurement("code.parameters", self.parameters),
            measurement("graph.incoming_edges", self.incoming_edges),
            measurement("graph.outgoing_edges", self.outgoing_edges),
            measurement("git.total_commits", self.git_total_commits),
            measurement("git.contributors", self.git_contributors),
            Measurement {
                name: "git.average_days_between_changes".to_owned(),
                value: self.git_average_days_between_changes,
                unit: Some("days".to_owned()),
            },
            Measurement {
                name: "test.line_coverage".to_owned(),
                value: self.test_line_coverage,
                unit: Some("ratio".to_owned()),
            },
            Measurement {
                name: "test.branch_coverage".to_owned(),
                value: self.test_branch_coverage,
                unit: Some("ratio".to_owned()),
            },
        ]
    }

    pub fn from_measurements(measurements: &[Measurement]) -> Option<Self> {
        Some(Self {
            lines_of_code: exact_u32(measurements, "code.lines_of_code")?,
            cyclomatic_complexity: exact_u32(measurements, "code.cyclomatic_complexity")?,
            parameters: exact_u32(measurements, "code.parameters")?,
            incoming_edges: exact_u32(measurements, "graph.incoming_edges")?,
            outgoing_edges: exact_u32(measurements, "graph.outgoing_edges")?,
            git_total_commits: exact_u32(measurements, "git.total_commits")?,
            git_contributors: exact_u32(measurements, "git.contributors")?,
            git_average_days_between_changes: value(
                measurements,
                "git.average_days_between_changes",
            )?,
            test_line_coverage: value(measurements, "test.line_coverage")?,
            test_branch_coverage: value(measurements, "test.branch_coverage")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl DependencyType {
    pub fn as_str(&self) -> String {
        match self {
            Self::Calls => "calls".to_owned(),
            Self::Imports => "imports".to_owned(),
            Self::References => "references".to_owned(),
            Self::Implements => "implements".to_owned(),
            Self::Inherits => "inherits".to_owned(),
            Self::Contains => "contains".to_owned(),
            Self::Other(value) => format!("other:{value}"),
        }
    }
}

pub fn symbol_observation(
    revision: &RevisionId,
    symbol: CodeSymbol,
    file_path: impl Into<String>,
    line_start: u32,
    line_end: u32,
    signature: Option<&str>,
    metrics: NodeMetrics,
) -> EntityObservation {
    let mut attributes = BTreeMap::new();
    attributes.insert("language".to_owned(), json!(symbol.language.as_str()));
    attributes.insert("qualified_name".to_owned(), json!(symbol.qualified_name));
    attributes.insert("file_path".to_owned(), json!(file_path.into()));
    attributes.insert("line_start".to_owned(), json!(line_start));
    attributes.insert("line_end".to_owned(), json!(line_end));
    attributes.insert("signature".to_owned(), json!(signature));

    EntityObservation {
        snapshot: revision.snapshot(),
        entity: Entity {
            id: EntityId::new(symbol.id.as_str()),
            model: ModelId::new(CODE_MODEL_ID),
            kind: symbol.kind.as_str().to_owned(),
            label: symbol.qualified_name,
        },
        attributes,
        measurements: metrics.measurements(),
        scores: Vec::new(),
    }
}

pub fn dependency_observation(
    revision: &RevisionId,
    from: &SymbolId,
    to: &SymbolId,
    relationship: &DependencyType,
    weight: f64,
) -> RelationObservation {
    let kind = relationship.as_str();
    RelationObservation {
        snapshot: revision.snapshot(),
        relation: Relation {
            id: RelationId::new(format!("{}:{kind}:{}", from.as_str(), to.as_str())),
            model: ModelId::new(CODE_MODEL_ID),
            kind,
            from: EntityId::new(from.as_str()),
            to: EntityId::new(to.as_str()),
        },
        weight,
        attributes: Attributes::new(),
    }
}

fn measurement(name: &str, value: u32) -> Measurement {
    Measurement {
        name: name.to_owned(),
        value: f64::from(value),
        unit: Some("count".to_owned()),
    }
}

fn value(measurements: &[Measurement], name: &str) -> Option<f64> {
    measurements
        .iter()
        .find(|measurement| measurement.name == name)
        .map(|measurement| measurement.value)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn exact_u32(measurements: &[Measurement], name: &str) -> Option<u32> {
    let value = value(measurements, name)?;
    (value.is_finite() && value >= 0.0 && value <= f64::from(u32::MAX) && value.fract() == 0.0)
        .then_some(value as u32)
}
