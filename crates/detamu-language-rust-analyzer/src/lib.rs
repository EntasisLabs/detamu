//! Optional rust-analyzer adapter for semantic references and call edges.

use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use detamu_core::{
    AnalysisCoverage, AnalysisDiagnostic, DiagnosticSeverity, ModelId, ObservationBatch,
    ObserverProvenance,
};
use detamu_language_lsp::{LspError, LspServerConfig, LspSession};
use detamu_model::{
    AnalysisInput, AnalyzerCapability, AnalyzerDescriptor, AnalyzerError, AnalyzerExecution,
    ArtifactContent, ArtifactReader, ModelAnalyzer,
};
use detamu_model_code::{
    CODE_MODEL_ID, DependencyType, GitOid, RepositoryId, RevisionId, SymbolId, acc_symbol_id,
    dependency_observation,
};
use serde_json::{Value, json};
use tokio::process::Command;
use url::Url;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct RustAnalyzer {
    artifacts: Arc<dyn ArtifactReader>,
    executable: PathBuf,
    request_timeout: Duration,
}

impl RustAnalyzer {
    pub fn new(artifacts: Arc<dyn ArtifactReader>) -> Self {
        Self {
            artifacts,
            executable: PathBuf::from("rust-analyzer"),
            request_timeout: Duration::from_secs(60),
        }
    }

    #[must_use]
    pub fn from_environment(artifacts: Arc<dyn ArtifactReader>) -> Self {
        let analyzer = Self::new(artifacts);
        match std::env::var_os("DETAMU_RUST_ANALYZER") {
            Some(executable) => analyzer.with_executable(executable),
            None => analyzer,
        }
    }

    #[must_use]
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = executable.into();
        self
    }

    pub async fn is_available(&self) -> bool {
        Command::new(&self.executable)
            .arg("--version")
            .output()
            .await
            .is_ok_and(|output| output.status.success())
    }

    async fn analyze_workspace(
        &self,
        input: &AnalysisInput,
        contents: Vec<ArtifactContent>,
    ) -> Result<ObservationBatch, AnalyzerError> {
        let workspace = ImmutableWorkspace::create(contents).await?;
        let root_uri = Url::from_directory_path(workspace.root())
            .map_err(|()| AnalyzerError::Failed("encode rust-analyzer root URI".to_owned()))?;
        let mut config = LspServerConfig::new(&self.executable);
        config.working_directory = Some(workspace.root().to_owned());
        config.root_uri = Some(root_uri.to_string());
        config.request_timeout = self.request_timeout;
        config.initialization_options = Some(json!({
            "cargo": { "buildScripts": { "enable": false } },
            "procMacro": { "enable": false },
        }));
        let mut session = LspSession::start(&config).await.map_err(analyzer_error)?;
        let result = observe_semantics(&mut session, input, workspace.root()).await;
        let shutdown = session.shutdown().await.map_err(analyzer_error);
        match (result, shutdown) {
            (Ok(batch), Ok(())) => Ok(batch),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

#[async_trait]
impl ModelAnalyzer for RustAnalyzer {
    fn descriptor(&self) -> AnalyzerDescriptor {
        AnalyzerDescriptor {
            name: "lsp.rust-analyzer".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            model: ModelId::new(CODE_MODEL_ID),
            capabilities: vec![AnalyzerCapability::References, AnalyzerCapability::Calls],
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
        self.analyze_workspace(input, contents).await
    }
}

async fn observe_semantics(
    session: &mut LspSession,
    input: &AnalysisInput,
    workspace: &Path,
) -> Result<ObservationBatch, AnalyzerError> {
    let revision = revision(input)?;
    let rust_files = rust_files(workspace).await?;
    let mut batch = ObservationBatch::empty(revision.snapshot());
    batch.coverage = AnalysisCoverage::Partial;
    batch.provenance.push(ObserverProvenance {
        observer: "lsp.rust-analyzer".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        configuration_digest: Some("references-calls-v1".to_owned()),
        source: None,
    });
    let mut catalog = Vec::new();
    for (index, path) in rust_files.iter().enumerate() {
        let uri = file_uri(path)?;
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(|error| AnalyzerError::Failed(format!("read Rust artifact: {error}")))?;
        session
            .notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": text,
                    }
                }),
            )
            .await
            .map_err(analyzer_error)?;
        match document_symbols(session, &uri, index == 0).await {
            Ok(symbols) => collect_document_symbols(&symbols, path, workspace, &mut catalog),
            Err(error) => batch.diagnostics.push(diagnostic(
                Some(relative_path(path, workspace)?),
                format!("document symbols unavailable: {error}"),
            )),
        }
    }

    // Document symbols are syntax-only and may arrive before rust-analyzer has
    // finished loading the crate graph needed by semantic requests.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut edges = BTreeSet::new();
    for symbol in &catalog {
        collect_references(session, symbol, &catalog, workspace, &mut edges, &mut batch).await?;
        if symbol.callable {
            collect_calls(session, symbol, &catalog, workspace, &mut edges, &mut batch).await?;
        }
    }
    for (kind, from, to) in edges {
        batch.relations.push(dependency_observation(
            &revision,
            &SymbolId::new(from),
            &SymbolId::new(to),
            &kind,
            1.0,
        ));
    }
    Ok(batch)
}

async fn document_symbols(
    session: &mut LspSession,
    uri: &str,
    wait_for_workspace: bool,
) -> Result<Value, LspError> {
    let attempts = if wait_for_workspace { 20 } else { 1 };
    let mut response = Value::Null;
    for attempt in 0..attempts {
        response = session
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri } }),
            )
            .await?;
        if response
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty())
            || attempt + 1 == attempts
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(response)
}

async fn collect_references(
    session: &mut LspSession,
    target: &SemanticSymbol,
    catalog: &[SemanticSymbol],
    workspace: &Path,
    edges: &mut BTreeSet<(DependencyType, String, String)>,
    batch: &mut ObservationBatch,
) -> Result<(), AnalyzerError> {
    let response = session
        .request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": target.uri },
                "position": target.selection_start,
                "context": { "includeDeclaration": false },
            }),
        )
        .await;
    let locations = match response {
        Ok(value) => value,
        Err(LspError::Server(_)) => return Ok(()),
        Err(error) => {
            batch.diagnostics.push(diagnostic(
                Some(target.path.clone()),
                format!("references unavailable for {}: {error}", target.name),
            ));
            return Ok(());
        }
    };
    for location in locations.as_array().into_iter().flatten() {
        let Some(uri) = location.get("uri").and_then(Value::as_str) else {
            continue;
        };
        let Some(start) = location.pointer("/range/start") else {
            continue;
        };
        let Some(source) = containing_symbol(catalog, uri, start, workspace)? else {
            continue;
        };
        if source.id != target.id {
            edges.insert((
                DependencyType::References,
                source.id.clone(),
                target.id.clone(),
            ));
        }
    }
    Ok(())
}

async fn collect_calls(
    session: &mut LspSession,
    source: &SemanticSymbol,
    catalog: &[SemanticSymbol],
    workspace: &Path,
    edges: &mut BTreeSet<(DependencyType, String, String)>,
    batch: &mut ObservationBatch,
) -> Result<(), AnalyzerError> {
    let prepared = match session
        .request(
            "textDocument/prepareCallHierarchy",
            json!({
                "textDocument": { "uri": source.uri },
                "position": source.selection_start,
            }),
        )
        .await
    {
        Ok(value) => value,
        Err(LspError::Server(_)) => return Ok(()),
        Err(error) => {
            batch.diagnostics.push(diagnostic(
                Some(source.path.clone()),
                format!("call hierarchy unavailable for {}: {error}", source.name),
            ));
            return Ok(());
        }
    };
    let Some(item) = prepared.as_array().and_then(|items| items.first()) else {
        return Ok(());
    };
    let outgoing = match session
        .request("callHierarchy/outgoingCalls", json!({ "item": item }))
        .await
    {
        Ok(value) => value,
        Err(LspError::Server(_)) => return Ok(()),
        Err(error) => {
            batch.diagnostics.push(diagnostic(
                Some(source.path.clone()),
                format!("outgoing calls unavailable for {}: {error}", source.name),
            ));
            return Ok(());
        }
    };
    for call in outgoing.as_array().into_iter().flatten() {
        let Some(target) = call.get("to") else {
            continue;
        };
        if let Some(target) = symbol_for_item(catalog, target, workspace)? {
            edges.insert((DependencyType::Calls, source.id.clone(), target.id.clone()));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct SemanticSymbol {
    id: String,
    name: String,
    path: String,
    uri: String,
    selection_start: Value,
    range: Value,
    callable: bool,
}

fn collect_document_symbols(
    value: &Value,
    path: &Path,
    workspace: &Path,
    catalog: &mut Vec<SemanticSymbol>,
) {
    let Some(symbols) = value.as_array() else {
        return;
    };
    for symbol in symbols {
        let Some(name) = symbol.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(kind) = symbol.get("kind").and_then(Value::as_u64) else {
            continue;
        };
        let range = symbol
            .get("range")
            .or_else(|| symbol.pointer("/location/range"));
        let selection = symbol.get("selectionRange").or(range);
        let Some(selection_start) = selection.and_then(|range| range.get("start")) else {
            continue;
        };
        let Some(line) = selection_start.get("line").and_then(Value::as_u64) else {
            continue;
        };
        let Ok(line) = u32::try_from(line.saturating_add(1)) else {
            continue;
        };
        if supported_symbol_kind(kind) {
            let relative = relative_path(path, workspace).unwrap_or_default();
            catalog.push(SemanticSymbol {
                id: acc_symbol_id(&relative, name, line).as_str().to_owned(),
                name: name.to_owned(),
                path: relative,
                uri: file_uri(path).unwrap_or_default(),
                selection_start: selection_start.clone(),
                range: range.cloned().unwrap_or(Value::Null),
                callable: matches!(kind, 6 | 9 | 12),
            });
        }
        if let Some(children) = symbol.get("children") {
            collect_document_symbols(children, path, workspace, catalog);
        }
    }
}

fn supported_symbol_kind(kind: u64) -> bool {
    matches!(kind, 2 | 5 | 6 | 9 | 10 | 11 | 12 | 23)
}

fn containing_symbol<'a>(
    catalog: &'a [SemanticSymbol],
    uri: &str,
    position: &Value,
    workspace: &Path,
) -> Result<Option<&'a SemanticSymbol>, AnalyzerError> {
    let path = uri_path(uri, workspace)?;
    Ok(catalog
        .iter()
        .filter(|symbol| symbol.path == path && contains(&symbol.range, position))
        .min_by_key(|symbol| range_span(&symbol.range)))
}

fn symbol_for_item<'a>(
    catalog: &'a [SemanticSymbol],
    item: &Value,
    workspace: &Path,
) -> Result<Option<&'a SemanticSymbol>, AnalyzerError> {
    let Some(uri) = item.get("uri").and_then(Value::as_str) else {
        return Ok(None);
    };
    let path = uri_path(uri, workspace)?;
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let start = item.pointer("/selectionRange/start");
    Ok(catalog.iter().find(|symbol| {
        symbol.path == path
            && symbol.name == name
            && start.is_some_and(|start| symbol.selection_start == *start)
    }))
}

fn contains(range: &Value, position: &Value) -> bool {
    let Some(start) = range.get("start") else {
        return false;
    };
    let Some(end) = range.get("end") else {
        return false;
    };
    compare_position(start, position).is_le() && compare_position(position, end).is_le()
}

fn compare_position(left: &Value, right: &Value) -> std::cmp::Ordering {
    let tuple = |position: &Value| {
        (
            position.get("line").and_then(Value::as_u64).unwrap_or(0),
            position
                .get("character")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
    };
    tuple(left).cmp(&tuple(right))
}

fn range_span(range: &Value) -> u64 {
    let start = range
        .pointer("/start/line")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let end = range
        .pointer("/end/line")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    end.saturating_sub(start)
}

async fn rust_files(workspace: &Path) -> Result<Vec<PathBuf>, AnalyzerError> {
    let mut pending = vec![workspace.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| AnalyzerError::Failed(format!("read workspace: {error}")))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| AnalyzerError::Failed(format!("read workspace entry: {error}")))?
        {
            let file_type = entry
                .file_type()
                .await
                .map_err(|error| AnalyzerError::Failed(format!("read artifact type: {error}")))?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if entry.path().extension().and_then(|value| value.to_str()) == Some("rs") {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

struct ImmutableWorkspace {
    root: PathBuf,
}

impl ImmutableWorkspace {
    async fn create(contents: Vec<ArtifactContent>) -> Result<Self, AnalyzerError> {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "detamu-rust-analyzer-{}-{sequence}",
            std::process::id()
        ));
        tokio::fs::create_dir_all(&root).await.map_err(|error| {
            AnalyzerError::Failed(format!("create semantic workspace: {error}"))
        })?;
        for content in contents {
            let relative = safe_relative_path(&content.artifact.path)?;
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    AnalyzerError::Failed(format!("create artifact directory: {error}"))
                })?;
            }
            tokio::fs::write(target, content.bytes)
                .await
                .map_err(|error| AnalyzerError::Failed(format!("write artifact: {error}")))?;
        }
        Ok(Self { root })
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ImmutableWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn safe_relative_path(path: &str) -> Result<&Path, AnalyzerError> {
    let path = Path::new(path);
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then_some(path)
        .ok_or_else(|| AnalyzerError::Failed(format!("unsafe artifact path: {}", path.display())))
}

fn file_uri(path: &Path) -> Result<String, AnalyzerError> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|()| AnalyzerError::Failed(format!("encode file URI: {}", path.display())))
}

fn uri_path(uri: &str, workspace: &Path) -> Result<String, AnalyzerError> {
    let path = Url::parse(uri)
        .map_err(|error| AnalyzerError::Failed(format!("parse LSP URI: {error}")))?
        .to_file_path()
        .map_err(|()| AnalyzerError::Failed(format!("LSP URI is not a file: {uri}")))?;
    relative_path(&path, workspace)
}

fn relative_path(path: &Path, workspace: &Path) -> Result<String, AnalyzerError> {
    path.strip_prefix(workspace)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            AnalyzerError::Failed(format!(
                "path escaped semantic workspace: {}",
                path.display()
            ))
        })
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

fn analyzer_error(error: LspError) -> AnalyzerError {
    match error {
        LspError::Unavailable(message) => AnalyzerError::Unavailable(message),
        other => AnalyzerError::Failed(other.to_string()),
    }
}

fn diagnostic(scope: Option<String>, message: String) -> AnalysisDiagnostic {
    AnalysisDiagnostic {
        severity: DiagnosticSeverity::Warning,
        observer: "lsp.rust-analyzer".to_owned(),
        message,
        scope,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use detamu_core::{SnapshotId, SnapshotVersion, WorldId};
    use detamu_model::{Artifact, ArtifactError, SourceReference};

    use super::*;

    struct FixtureReader;

    #[async_trait]
    impl ArtifactReader for FixtureReader {
        fn supports(&self, source: &SourceReference) -> bool {
            source.kind == "fixture"
        }

        async fn artifacts(
            &self,
            _source: &SourceReference,
        ) -> Result<Vec<Artifact>, ArtifactError> {
            Ok(vec![
                Artifact {
                    path: "Cargo.toml".to_owned(),
                    content_id: "manifest".to_owned(),
                    media_type: None,
                    attributes: BTreeMap::new(),
                },
                Artifact {
                    path: "src/lib.rs".to_owned(),
                    content_id: "source".to_owned(),
                    media_type: Some("text/x-rust".to_owned()),
                    attributes: BTreeMap::new(),
                },
            ])
        }

        async fn read_many(
            &self,
            _source: &SourceReference,
            artifacts: &[Artifact],
        ) -> Result<Vec<ArtifactContent>, ArtifactError> {
            Ok(artifacts
                .iter()
                .cloned()
                .map(|artifact| {
                    let bytes = if artifact.path == "Cargo.toml" {
                        b"[package]\nname='semantic-fixture'\nversion='0.1.0'\nedition='2024'\n"
                            .to_vec()
                    } else {
                        b"pub fn target() {}\npub fn source() { target(); }\n".to_vec()
                    };
                    ArtifactContent { artifact, bytes }
                })
                .collect())
        }
    }

    #[test]
    fn normalizes_document_symbols_to_acc_identity() {
        let workspace = Path::new("/tmp/detamu-ra-fixture");
        let path = workspace.join("src/lib.rs");
        let symbols = json!([{
            "name": "run",
            "kind": 12,
            "range": {
                "start": { "line": 2, "character": 0 },
                "end": { "line": 4, "character": 1 }
            },
            "selectionRange": {
                "start": { "line": 2, "character": 7 },
                "end": { "line": 2, "character": 10 }
            }
        }]);
        let mut catalog = Vec::new();
        collect_document_symbols(&symbols, &path, workspace, &mut catalog);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].path, "src/lib.rs");
        assert_eq!(
            catalog[0].id,
            acc_symbol_id("src/lib.rs", "run", 3).as_str()
        );
        assert!(catalog[0].callable);
    }

    #[test]
    fn selects_the_innermost_symbol_containing_a_reference() {
        let catalog = vec![
            SemanticSymbol {
                id: "outer".to_owned(),
                name: "outer".to_owned(),
                path: "src/lib.rs".to_owned(),
                uri: "file:///tmp/detamu-ra-fixture/src/lib.rs".to_owned(),
                selection_start: json!({ "line": 0, "character": 3 }),
                range: json!({
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 10, "character": 0 }
                }),
                callable: true,
            },
            SemanticSymbol {
                id: "inner".to_owned(),
                name: "inner".to_owned(),
                path: "src/lib.rs".to_owned(),
                uri: "file:///tmp/detamu-ra-fixture/src/lib.rs".to_owned(),
                selection_start: json!({ "line": 2, "character": 3 }),
                range: json!({
                    "start": { "line": 2, "character": 0 },
                    "end": { "line": 4, "character": 0 }
                }),
                callable: true,
            },
        ];
        let selected = containing_symbol(
            &catalog,
            "file:///tmp/detamu-ra-fixture/src/lib.rs",
            &json!({ "line": 3, "character": 1 }),
            Path::new("/tmp/detamu-ra-fixture"),
        )
        .expect("map location")
        .expect("containing symbol");
        assert_eq!(selected.id, "inner");
    }

    #[tokio::test]
    async fn live_rust_analyzer_emits_call_edges_when_configured() {
        if std::env::var_os("DETAMU_RUST_ANALYZER").is_none() {
            return;
        }
        let analyzer = RustAnalyzer::from_environment(Arc::new(FixtureReader));
        let batch = analyzer
            .analyze(&AnalysisInput {
                snapshot: SnapshotId::new(
                    WorldId::new("code.repository:fixture"),
                    SnapshotVersion::new("abc"),
                ),
                sources: vec![SourceReference {
                    kind: "fixture".to_owned(),
                    locator: "fixture".to_owned(),
                    cursor: Some("abc".to_owned()),
                    attributes: BTreeMap::new(),
                }],
                changed_entities: None,
            })
            .await
            .expect("run configured rust-analyzer");
        assert!(
            batch
                .relations
                .iter()
                .any(|relation| relation.relation.kind == "calls"),
            "semantic batch: {batch:#?}"
        );
    }
}
