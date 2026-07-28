//! Optional process adapter for Lizard's broad language metrics.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use detamu_core::{AnalysisCoverage, ModelId, ObservationBatch, ObserverProvenance};
use detamu_model::{
    AnalysisInput, AnalyzerCapability, AnalyzerDescriptor, AnalyzerError, AnalyzerExecution,
    Artifact, ArtifactReader, ModelAnalyzer,
};
use detamu_model_code::{
    CODE_MODEL_ID, CodeSymbol, GitOid, LanguageId, NodeKind, RepositoryId, RevisionId,
    SymbolLocation, SyntaxMetrics, acc_symbol_id, file_contains_symbol, syntax_symbol_observation,
};
use tokio::process::Command;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct LizardAnalyzer {
    artifacts: Arc<dyn ArtifactReader>,
    executable: PathBuf,
}

impl LizardAnalyzer {
    pub fn new(artifacts: Arc<dyn ArtifactReader>) -> Self {
        Self::with_executable(artifacts, "lizard")
    }

    pub fn with_executable(
        artifacts: Arc<dyn ArtifactReader>,
        executable: impl Into<PathBuf>,
    ) -> Self {
        Self {
            artifacts,
            executable: executable.into(),
        }
    }

    pub async fn is_available(&self) -> bool {
        Command::new(&self.executable)
            .arg("--version")
            .output()
            .await
            .is_ok_and(|output| output.status.success())
    }
}

#[async_trait]
impl ModelAnalyzer for LizardAnalyzer {
    fn descriptor(&self) -> AnalyzerDescriptor {
        AnalyzerDescriptor {
            name: "lizard".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            model: ModelId::new(CODE_MODEL_ID),
            capabilities: vec![AnalyzerCapability::Symbols, AnalyzerCapability::Metrics],
            execution: AnalyzerExecution::Optional,
        }
    }

    async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
        if !self.is_available().await {
            return Err(AnalyzerError::Unavailable(format!(
                "{} is not installed or executable",
                self.executable.display()
            )));
        }
        let source = input
            .sources
            .iter()
            .find(|source| self.artifacts.supports(source))
            .ok_or_else(|| AnalyzerError::Unavailable("artifact source is missing".to_owned()))?;
        let artifacts = self
            .artifacts
            .artifacts(source)
            .await
            .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
        let contents = self
            .artifacts
            .read_many(source, &artifacts)
            .await
            .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
        let temporary = temporary_directory();
        tokio::fs::create_dir_all(&temporary)
            .await
            .map_err(|error| AnalyzerError::Failed(format!("create Lizard workspace: {error}")))?;
        for content in contents {
            let relative = safe_relative_path(&content.artifact.path)?;
            let target = temporary.join(relative);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    AnalyzerError::Failed(format!("create Lizard artifact directory: {error}"))
                })?;
            }
            tokio::fs::write(&target, content.bytes)
                .await
                .map_err(|error| {
                    AnalyzerError::Failed(format!("materialize Lizard artifact: {error}"))
                })?;
        }

        let output = Command::new(&self.executable)
            .arg("--csv")
            .arg(&temporary)
            .output()
            .await
            .map_err(|error| AnalyzerError::Unavailable(format!("run Lizard: {error}")))?;
        let cleanup = tokio::fs::remove_dir_all(&temporary).await;
        if !output.status.success() {
            return Err(AnalyzerError::Failed(format!(
                "Lizard exited unsuccessfully: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        cleanup.map_err(|error| {
            AnalyzerError::Failed(format!("remove temporary Lizard workspace: {error}"))
        })?;
        parse_csv(input, &temporary, &artifacts, &output.stdout)
    }
}

fn parse_csv(
    input: &AnalysisInput,
    temporary: &Path,
    artifacts: &[Artifact],
    output: &[u8],
) -> Result<ObservationBatch, AnalyzerError> {
    let revision = revision(input)?;
    let mut batch = ObservationBatch::empty(revision.snapshot());
    batch.coverage = AnalysisCoverage::Partial;
    batch.provenance.push(ObserverProvenance {
        observer: "lizard".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        configuration_digest: Some("lizard-csv-v1".to_owned()),
        source: None,
    });
    let languages = artifacts
        .iter()
        .map(|artifact| (artifact.path.as_str(), artifact_language(artifact)))
        .collect::<BTreeMap<_, _>>();
    let records = csv_records(output)?;
    let (first, remaining) = records
        .split_first()
        .ok_or_else(|| AnalyzerError::Failed("Lizard returned empty CSV output".to_owned()))?;
    // Lizard's documented CSV layout is headerless, while some wrappers and
    // older fixtures include a header row. Accept both without making the
    // process adapter depend on a particular Lizard distribution.
    let (columns, records) = if first
        .first()
        .is_some_and(|value| value.eq_ignore_ascii_case("NLOC"))
    {
        (CsvColumns::from_headers(first)?, remaining)
    } else {
        (CsvColumns::lizard_default(), records.as_slice())
    };
    for record in records {
        let Some(row) = columns.parse(record, temporary)? else {
            continue;
        };
        let language = languages
            .get(row.path.as_str())
            .cloned()
            .flatten()
            .unwrap_or_else(|| LanguageId::new("unknown"));
        // Lizard does not expose enough ownership information to distinguish
        // Rust free functions from impl methods. The Rust Tree-sitter pack is
        // authoritative for kind and will replace this unknown during merge.
        let kind = if language.as_str() == "rust" {
            NodeKind::Unknown
        } else if row.qualified_name.contains("::") || row.qualified_name.contains('.') {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let qualified_name = if language.as_str() == "rust" {
            row.name.clone()
        } else {
            row.qualified_name
        };
        let id = acc_symbol_id(&row.path, &row.name, row.start);
        let mut observation = syntax_symbol_observation(
            &revision,
            CodeSymbol {
                id: id.clone(),
                language,
                qualified_name,
                kind,
            },
            SymbolLocation {
                file_path: &row.path,
                line_start: row.start,
                line_end: row.end,
                signature: None,
            },
            SyntaxMetrics {
                lines_of_code: row.nloc,
                cyclomatic_complexity: row.complexity,
                parameters: row.parameters,
            },
            None,
            "lizard",
            0.8,
        );
        observation.attributes.remove("signature");
        observation.attributes.remove("qualified_name");
        if let Some(line_end) = observation.attributes.remove("line_end") {
            observation
                .attributes
                .insert("lizard.line_end".to_owned(), line_end);
        }
        batch.entities.push(observation);
        batch
            .relations
            .push(file_contains_symbol(&revision, &row.path, &id));
    }
    Ok(batch)
}

struct CsvColumns {
    nloc: usize,
    complexity: usize,
    parameters: usize,
    file: usize,
    function: usize,
    long_name: usize,
    start: usize,
    end: usize,
}

impl CsvColumns {
    const fn lizard_default() -> Self {
        Self {
            nloc: 0,
            complexity: 1,
            parameters: 3,
            file: 6,
            function: 7,
            long_name: 8,
            start: 9,
            end: 10,
        }
    }

    fn from_headers(headers: &[String]) -> Result<Self, AnalyzerError> {
        let index = |names: &[&str]| {
            headers
                .iter()
                .position(|header| names.iter().any(|name| header.eq_ignore_ascii_case(name)))
                .ok_or_else(|| AnalyzerError::Failed(format!("Lizard CSV lacks {}", names[0])))
        };
        Ok(Self {
            nloc: index(&["NLOC"])?,
            complexity: index(&["CCN", "cyclomatic_complexity"])?,
            parameters: index(&["PARAM", "parameter_count"])?,
            file: index(&["file", "filename"])?,
            function: index(&["function", "name"])?,
            long_name: index(&["long_name", "long name"])?,
            start: index(&["start", "start_line"])?,
            end: index(&["end", "end_line"])?,
        })
    }

    fn parse(
        &self,
        record: &[String],
        temporary: &Path,
    ) -> Result<Option<LizardRow>, AnalyzerError> {
        let Some(file) = record.get(self.file) else {
            return Ok(None);
        };
        let path = Path::new(file)
            .strip_prefix(temporary)
            .unwrap_or_else(|_| Path::new(file))
            .to_string_lossy()
            .trim_start_matches('/')
            .to_owned();
        let value = |column: usize, name: &str| -> Result<u32, AnalyzerError> {
            record
                .get(column)
                .ok_or_else(|| AnalyzerError::Failed(format!("Lizard CSV lacks {name}")))?
                .parse()
                .map_err(|error| AnalyzerError::Failed(format!("invalid Lizard {name}: {error}")))
        };
        let name = record.get(self.function).map_or("", String::as_str).trim();
        if name.is_empty() {
            return Ok(None);
        }
        let qualified_name = record
            .get(self.long_name)
            .map_or(name, String::as_str)
            .split('(')
            .next()
            .unwrap_or(name)
            .trim()
            .to_owned();
        Ok(Some(LizardRow {
            path,
            name: name.to_owned(),
            qualified_name,
            nloc: value(self.nloc, "NLOC")?,
            complexity: value(self.complexity, "CCN")?,
            parameters: value(self.parameters, "PARAM")?,
            start: value(self.start, "start")?,
            end: value(self.end, "end")?,
        }))
    }
}

fn csv_records(output: &[u8]) -> Result<Vec<Vec<String>>, AnalyzerError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| AnalyzerError::Failed("Lizard returned non-UTF-8 CSV".to_owned()))?;
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                field.push('"');
                characters.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => record.push(std::mem::take(&mut field)),
            '\n' if !quoted => {
                if field.ends_with('\r') {
                    field.pop();
                }
                record.push(std::mem::take(&mut field));
                if record.iter().any(|value| !value.is_empty()) {
                    records.push(std::mem::take(&mut record));
                } else {
                    record.clear();
                }
            }
            _ => field.push(character),
        }
    }
    if quoted {
        return Err(AnalyzerError::Failed(
            "Lizard returned malformed quoted CSV".to_owned(),
        ));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

struct LizardRow {
    path: String,
    name: String,
    qualified_name: String,
    nloc: u32,
    complexity: u32,
    parameters: u32,
    start: u32,
    end: u32,
}

fn artifact_language(artifact: &Artifact) -> Option<LanguageId> {
    artifact
        .attributes
        .get("language")
        .and_then(serde_json::Value::as_str)
        .map(LanguageId::new)
}

fn safe_relative_path(path: &str) -> Result<&Path, AnalyzerError> {
    let path = Path::new(path);
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(path)
    } else {
        Err(AnalyzerError::Failed(format!(
            "unsafe artifact path: {}",
            path.display()
        )))
    }
}

fn temporary_directory() -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("detamu-lizard-{}-{sequence}", std::process::id()))
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

#[cfg(test)]
mod tests {
    use detamu_core::{SnapshotId, SnapshotVersion, WorldId};

    use super::*;

    #[test]
    fn parses_lizard_csv_into_normalized_evidence() {
        let temporary = Path::new("/tmp/lizard-fixture");
        let csv = b"NLOC,CCN,token,PARAM,length,location,file,function,long_name,start,end\n4,3,10,2,4,x,/tmp/lizard-fixture/src/lib.rs,greet,Greeter::greet(bool),4,7\n";
        let mut attributes = BTreeMap::new();
        attributes.insert("language".to_owned(), serde_json::json!("rust"));
        let input = AnalysisInput {
            snapshot: SnapshotId::new(
                WorldId::new("code.repository:fixture"),
                SnapshotVersion::new("abc"),
            ),
            sources: Vec::new(),
            changed_entities: None,
        };
        let mut batch = parse_csv(
            &input,
            temporary,
            &[Artifact {
                path: "src/lib.rs".to_owned(),
                content_id: "blob".to_owned(),
                media_type: None,
                attributes,
            }],
            csv,
        )
        .expect("parse fixture");
        assert_eq!(batch.entities.len(), 1);
        assert_eq!(batch.entities[0].entity.label, "greet");
        assert!((batch.entities[0].measurements[1].value - 3.0).abs() < f64::EPSILON);
        assert_eq!(
            batch.entities[0].measurements[1]
                .evidence
                .as_ref()
                .map(|evidence| evidence.observer.as_str()),
            Some("lizard")
        );

        let revision = RevisionId::new(RepositoryId::new("fixture"), GitOid::new("abc"));
        let symbol_id = acc_symbol_id("src/lib.rs", "greet", 4);
        let mut tree_sitter = ObservationBatch::empty(revision.snapshot());
        tree_sitter.entities.push(syntax_symbol_observation(
            &revision,
            CodeSymbol {
                id: symbol_id.clone(),
                language: LanguageId::new("rust"),
                qualified_name: "Greeter::greet".to_owned(),
                kind: NodeKind::Method,
            },
            SymbolLocation {
                file_path: "src/lib.rs",
                line_start: 4,
                line_end: 7,
                signature: Some("fn greet(&self, ready: bool) -> u8"),
            },
            SyntaxMetrics {
                lines_of_code: 4,
                cyclomatic_complexity: 2,
                parameters: 2,
            },
            None,
            "treesitter.rust",
            0.9,
        ));
        tree_sitter
            .relations
            .push(file_contains_symbol(&revision, "src/lib.rs", &symbol_id));
        batch.merge(tree_sitter).expect("merge analyzer evidence");
        assert_eq!(batch.entities.len(), 1);
        assert_eq!(batch.relations.len(), 1);
        assert_eq!(batch.entities[0].measurements.len(), 6);
    }

    #[test]
    fn parses_headerless_lizard_csv() {
        let temporary = Path::new("/tmp/lizard-fixture");
        let csv = b"4,3,10,2,4,x,/tmp/lizard-fixture/src/lib.rs,greet,Greeter::greet(bool),4,7\n";
        let mut attributes = BTreeMap::new();
        attributes.insert("language".to_owned(), serde_json::json!("rust"));
        let input = AnalysisInput {
            snapshot: SnapshotId::new(
                WorldId::new("code.repository:fixture"),
                SnapshotVersion::new("abc"),
            ),
            sources: Vec::new(),
            changed_entities: None,
        };
        let batch = parse_csv(
            &input,
            temporary,
            &[Artifact {
                path: "src/lib.rs".to_owned(),
                content_id: "blob".to_owned(),
                media_type: None,
                attributes,
            }],
            csv,
        )
        .expect("parse headerless fixture");

        assert_eq!(batch.entities.len(), 1);
        assert_eq!(batch.entities[0].entity.label, "greet");
        assert!((batch.entities[0].measurements[1].value - 3.0).abs() < f64::EPSILON);
    }
}
