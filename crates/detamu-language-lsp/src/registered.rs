//! Launch any stdio language server and normalize its graph into the code model.

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
    AnalysisCoverage, AnalysisDiagnostic, Attributes, DiagnosticSeverity, Entity, EntityId,
    EntityObservation, ObservationBatch, ObserverProvenance, Relation, RelationId,
    RelationObservation,
};
use detamu_model::{
    ARTIFACT_READ_BUDGET_BYTES, AnalysisInput, AnalyzerCapability, AnalyzerDescriptor,
    AnalyzerError, AnalyzerExecution, ArtifactContent, ArtifactReader, ModelAnalyzer,
    artifact_read_groups,
};
use detamu_model_code::{
    CODE_MODEL_ID, DependencyType, GitOid, NodeKind, RepositoryId, RevisionId, SymbolId,
    acc_symbol_id, dependency_observation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::{LspError, LspServerConfig, LspSession};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A user-registered language server that Detamu launches over stdio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspRegistration {
    pub id: String,
    pub language: String,
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub initialization_options: Option<Value>,
}

impl LspRegistration {
    /// Normalizes identifiers and rejects a registration that cannot be launched.
    ///
    /// # Errors
    ///
    /// Returns an error when the id, language, command, or extensions are empty.
    pub fn prepare(mut self) -> Result<Self, String> {
        let id = token(&self.id, "id")?;
        self.id = id;
        self.language = self.language.trim().to_ascii_lowercase();
        if self.language.is_empty() {
            return Err("language is required".to_owned());
        }
        self.command = self.command.trim().to_owned();
        if self.command.is_empty() {
            return Err("command is required".to_owned());
        }
        self.extensions = self
            .extensions
            .iter()
            .map(|extension| {
                extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase()
            })
            .filter(|extension| !extension.is_empty())
            .collect();
        self.extensions.sort();
        self.extensions.dedup();
        if self.extensions.is_empty() {
            return Err("at least one file extension is required".to_owned());
        }
        Ok(self)
    }

    fn matches_path(&self, path: &str) -> bool {
        let Some(extension) = Path::new(path).extension().and_then(|value| value.to_str()) else {
            return false;
        };
        self.extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(extension))
    }
}

/// Indexes one immutable snapshot by driving a registered language server.
pub struct RegisteredLsp {
    artifacts: Arc<dyn ArtifactReader>,
    registration: LspRegistration,
}

impl RegisteredLsp {
    pub fn new(artifacts: Arc<dyn ArtifactReader>, registration: LspRegistration) -> Self {
        Self {
            artifacts,
            registration,
        }
    }

    fn observer(&self) -> String {
        format!("lsp.{}", self.registration.id)
    }
}

#[async_trait]
impl ModelAnalyzer for RegisteredLsp {
    fn descriptor(&self) -> AnalyzerDescriptor {
        AnalyzerDescriptor {
            name: self.observer(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            model: detamu_core::ModelId::new(CODE_MODEL_ID),
            capabilities: vec![
                AnalyzerCapability::Symbols,
                AnalyzerCapability::References,
                AnalyzerCapability::Calls,
                AnalyzerCapability::Types,
            ],
            execution: AnalyzerExecution::Optional,
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
            .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
        let matching = artifacts
            .iter()
            .filter(|artifact| self.registration.matches_path(&artifact.path))
            .map(|artifact| artifact.path.clone())
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Ok(empty_batch(
                &input.snapshot,
                &self.observer(),
                "no tracked files matched the registered extensions",
            ));
        }
        let workspace = MaterializedTree::create(&self.registration.id).await?;
        for group in artifact_read_groups(&artifacts, ARTIFACT_READ_BUDGET_BYTES) {
            let contents = self
                .artifacts
                .read_many(source, &artifacts[group])
                .await
                .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
            workspace.write_contents(contents).await?;
        }
        self.observe(input, &workspace, &matching).await
    }
}

impl RegisteredLsp {
    async fn observe(
        &self,
        input: &AnalysisInput,
        workspace: &MaterializedTree,
        files: &[String],
    ) -> Result<ObservationBatch, AnalyzerError> {
        let revision = revision(input)?;
        let root_uri = Url::from_directory_path(workspace.root())
            .map_err(|()| AnalyzerError::Failed("encode language server root URI".to_owned()))?;
        let mut config = LspServerConfig::new(&self.registration.command);
        config.arguments.clone_from(&self.registration.arguments);
        config.working_directory = Some(workspace.root().to_owned());
        config.root_uri = Some(root_uri.to_string());
        config.request_timeout = REQUEST_TIMEOUT;
        config.initialization_options = self.registration.initialization_options.clone();
        let mut session = LspSession::start(&config).await.map_err(analyzer_error)?;
        let observed = self
            .collect(&mut session, &revision, workspace.root(), files)
            .await;
        let shutdown = session.shutdown().await.map_err(analyzer_error);
        match (observed, shutdown) {
            (Ok(batch), Ok(())) => Ok(batch),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn collect(
        &self,
        session: &mut LspSession,
        revision: &RevisionId,
        workspace: &Path,
        files: &[String],
    ) -> Result<ObservationBatch, AnalyzerError> {
        let mut catalog = Vec::new();
        let mut diagnostics = Vec::new();
        for (index, relative) in files.iter().enumerate() {
            let path = workspace.join(relative);
            let uri = file_uri(&path)?;
            let text = match tokio::fs::read_to_string(&path).await {
                Ok(text) => text,
                Err(error) => {
                    diagnostics.push(diagnostic(
                        &self.observer(),
                        Some(relative.clone()),
                        format!("language server input is not UTF-8 text: {error}"),
                    ));
                    continue;
                }
            };
            session
                .notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": self.registration.language,
                            "version": 1,
                            "text": text,
                        }
                    }),
                )
                .await
                .map_err(analyzer_error)?;
            match document_symbols(session, &uri, index == 0).await {
                Ok(symbols) => collect_symbols(&symbols, relative, &uri, &mut catalog),
                Err(error) => diagnostics.push(diagnostic(
                    &self.observer(),
                    Some(relative.clone()),
                    format!("document symbols unavailable: {error}"),
                )),
            }
        }

        let mut edges = BTreeSet::new();
        let mut unresolved = BTreeSet::new();
        for symbol in &catalog {
            collect_references(
                session,
                symbol,
                &catalog,
                &self.observer(),
                &mut edges,
                &mut diagnostics,
            )
            .await?;
            if symbol.callable {
                collect_calls(
                    session,
                    symbol,
                    &catalog,
                    workspace,
                    &self.observer(),
                    &mut edges,
                    &mut diagnostics,
                )
                .await?;
            }
            collect_bases(
                symbol,
                &catalog,
                &self.registration.language,
                &mut edges,
                &mut unresolved,
            );
        }
        let observer = self.observer();
        Ok(assemble(
            &BatchContext {
                revision,
                language: &self.registration.language,
                observer: &observer,
                registration_id: &self.registration.id,
            },
            &catalog,
            &edges,
            &unresolved,
            diagnostics,
        ))
    }
}

#[derive(Debug, Clone)]
struct CatalogSymbol {
    id: String,
    name: String,
    kind: NodeKind,
    path: String,
    line_start: u32,
    line_end: u32,
    namespace: Option<String>,
    signature: Option<String>,
    callable: bool,
    selection_start: Value,
    range: Value,
    uri: String,
    detail: Option<String>,
}

struct BatchContext<'a> {
    revision: &'a RevisionId,
    language: &'a str,
    observer: &'a str,
    registration_id: &'a str,
}

fn assemble(
    context: &BatchContext<'_>,
    catalog: &[CatalogSymbol],
    edges: &BTreeSet<(DependencyType, String, String)>,
    unresolved: &BTreeSet<(String, String)>,
    diagnostics: Vec<AnalysisDiagnostic>,
) -> ObservationBatch {
    let mut batch = ObservationBatch::empty(context.revision.snapshot());
    batch.coverage = AnalysisCoverage::Partial;
    batch.provenance.push(ObserverProvenance {
        observer: context.observer.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        configuration_digest: Some(context.registration_id.to_owned()),
        source: Some(context.language.to_owned()),
    });
    batch.diagnostics = diagnostics;
    let mut files = BTreeSet::new();
    for symbol in catalog {
        if files.insert(symbol.path.clone()) {
            batch.entities.push(file_entity(
                context.revision,
                &symbol.path,
                context.language,
            ));
        }
        batch
            .entities
            .push(symbol_entity(context.revision, context.language, symbol));
        batch.relations.push(contains_relation(
            context.revision,
            &symbol.path,
            &symbol.id,
        ));
    }
    for (kind, from, to) in edges {
        if let Some((_, name)) = unresolved.iter().find(|(id, _)| id == to) {
            ensure_unresolved(&mut batch, context.revision, context.language, to, name);
        }
        batch.relations.push(dependency_observation(
            context.revision,
            &SymbolId::new(from),
            &SymbolId::new(to),
            kind,
        ));
    }
    batch
}

fn ensure_unresolved(
    batch: &mut ObservationBatch,
    revision: &RevisionId,
    language: &str,
    id: &str,
    name: &str,
) {
    if batch
        .entities
        .iter()
        .any(|entity| entity.entity.id.as_str() == id)
    {
        return;
    }
    let mut attributes = Attributes::new();
    attributes.insert("language".to_owned(), json!(language));
    attributes.insert("resolution".to_owned(), json!("unresolved"));
    attributes.insert("qualified_name".to_owned(), json!(name));
    batch.entities.push(EntityObservation {
        snapshot: revision.snapshot(),
        entity: Entity {
            id: EntityId::new(id),
            model: model_id(),
            kind: NodeKind::Type.as_str().to_owned(),
            label: name.to_owned(),
        },
        attributes,
        measurements: Vec::new(),
        scores: Vec::new(),
    });
}

fn collect_symbols(value: &Value, path: &str, uri: &str, catalog: &mut Vec<CatalogSymbol>) {
    walk_symbols(value, path, uri, None, catalog);
}

fn walk_symbols(
    value: &Value,
    path: &str,
    uri: &str,
    namespace: Option<&str>,
    catalog: &mut Vec<CatalogSymbol>,
) {
    let Some(symbols) = value.as_array() else {
        return;
    };
    for symbol in symbols {
        let Some(name) = symbol.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(lsp_kind) = symbol.get("kind").and_then(Value::as_u64) else {
            continue;
        };
        let range = symbol
            .get("range")
            .or_else(|| symbol.pointer("/location/range"));
        let selection = symbol.get("selectionRange").or(range);
        let Some(selection_start) = selection.and_then(|range| range.get("start")) else {
            continue;
        };
        let child_namespace = namespace_for_child(namespace, name, lsp_kind);
        if let Some(kind) = code_kind(lsp_kind)
            && let Some(line_start) = one_based_line(selection_start)
        {
            let line_end = range
                .and_then(|range| range.pointer("/end/line"))
                .and_then(one_based_line)
                .unwrap_or(line_start);
            catalog.push(CatalogSymbol {
                id: acc_symbol_id(path, name, line_start).as_str().to_owned(),
                name: name.to_owned(),
                kind,
                path: path.to_owned(),
                line_start,
                line_end,
                namespace: namespace.map(str::to_owned),
                signature: symbol
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                callable: matches!(lsp_kind, 6 | 9 | 12),
                selection_start: selection_start.clone(),
                range: range.cloned().unwrap_or(Value::Null),
                uri: uri.to_owned(),
                detail: symbol
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            });
        }
        if let Some(children) = symbol.get("children") {
            walk_symbols(children, path, uri, child_namespace.as_deref(), catalog);
        }
    }
}

fn namespace_for_child(namespace: Option<&str>, name: &str, kind: u64) -> Option<String> {
    if !matches!(kind, 2..=4) {
        return namespace.map(str::to_owned);
    }
    Some(match namespace {
        Some(parent) if !parent.is_empty() => format!("{parent}.{name}"),
        _ => name.to_owned(),
    })
}

fn code_kind(kind: u64) -> Option<NodeKind> {
    Some(match kind {
        2 | 4 => NodeKind::Module,
        3 => NodeKind::Namespace,
        5 | 10 | 23 => NodeKind::Type,
        11 => NodeKind::Interface,
        6 | 9 => NodeKind::Method,
        12 => NodeKind::Function,
        7 | 8 => NodeKind::Field,
        14 | 22 => NodeKind::Constant,
        _ => return None,
    })
}

fn one_based_line(position: &Value) -> Option<u32> {
    let line = position.get("line").and_then(Value::as_u64)?;
    u32::try_from(line.saturating_add(1)).ok()
}

fn collect_bases(
    symbol: &CatalogSymbol,
    catalog: &[CatalogSymbol],
    language: &str,
    edges: &mut BTreeSet<(DependencyType, String, String)>,
    unresolved: &mut BTreeSet<(String, String)>,
) {
    if !matches!(symbol.kind, NodeKind::Type | NodeKind::Interface) {
        return;
    }
    let Some(detail) = &symbol.detail else {
        return;
    };
    for base in base_names(detail) {
        let kind = if language.eq_ignore_ascii_case("csharp") && interface_name(&base) {
            DependencyType::Implements
        } else {
            DependencyType::Inherits
        };
        let target = catalog
            .iter()
            .find(|candidate| candidate.name == base && candidate.path == symbol.path)
            .or_else(|| catalog.iter().find(|candidate| candidate.name == base))
            .map_or_else(
                || {
                    let id = unresolved_id(language, &base);
                    unresolved.insert((id.clone(), base.clone()));
                    id
                },
                |candidate| candidate.id.clone(),
            );
        if target != symbol.id {
            edges.insert((kind, symbol.id.clone(), target));
        }
    }
}

fn base_names(detail: &str) -> Vec<String> {
    let Some((_, rest)) = detail.split_once(':') else {
        return Vec::new();
    };
    if rest.contains('(') {
        return Vec::new();
    }
    rest.split(',')
        .filter_map(|part| {
            let name = part.split('<').next().unwrap_or(part).trim();
            let name = name.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '_' && character != '.'
            });
            let name = name.rsplit('.').next().unwrap_or(name);
            (name.chars().next().is_some_and(char::is_uppercase)).then(|| name.to_owned())
        })
        .collect()
}

fn interface_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters.next() == Some('I') && characters.next().is_some_and(char::is_uppercase)
}

fn unresolved_id(language: &str, name: &str) -> String {
    acc_symbol_id(&format!("unresolved/{language}"), name, 1)
        .as_str()
        .to_owned()
}

async fn collect_references(
    session: &mut LspSession,
    target: &CatalogSymbol,
    catalog: &[CatalogSymbol],
    observer: &str,
    edges: &mut BTreeSet<(DependencyType, String, String)>,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) -> Result<(), AnalyzerError> {
    let locations = match session
        .request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": target.uri },
                "position": target.selection_start,
                "context": { "includeDeclaration": false },
            }),
        )
        .await
    {
        Ok(value) => value,
        Err(LspError::Server(_)) => return Ok(()),
        Err(error) => {
            diagnostics.push(diagnostic(
                observer,
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
        let Some(source) = containing_symbol(catalog, uri, start) else {
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
    source: &CatalogSymbol,
    catalog: &[CatalogSymbol],
    workspace: &Path,
    observer: &str,
    edges: &mut BTreeSet<(DependencyType, String, String)>,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
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
            diagnostics.push(diagnostic(
                observer,
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
            diagnostics.push(diagnostic(
                observer,
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

fn containing_symbol<'a>(
    catalog: &'a [CatalogSymbol],
    uri: &str,
    position: &Value,
) -> Option<&'a CatalogSymbol> {
    catalog
        .iter()
        .filter(|symbol| symbol.uri == uri && contains(&symbol.range, position))
        .min_by_key(|symbol| range_span(&symbol.range))
}

fn symbol_for_item<'a>(
    catalog: &'a [CatalogSymbol],
    item: &Value,
    workspace: &Path,
) -> Result<Option<&'a CatalogSymbol>, AnalyzerError> {
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

fn symbol_entity(
    revision: &RevisionId,
    language: &str,
    symbol: &CatalogSymbol,
) -> EntityObservation {
    let qualified = match &symbol.namespace {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}.{}", symbol.name),
        _ => symbol.name.clone(),
    };
    let mut attributes = Attributes::new();
    attributes.insert("language".to_owned(), json!(language));
    attributes.insert("qualified_name".to_owned(), json!(qualified));
    attributes.insert("file_path".to_owned(), json!(symbol.path));
    attributes.insert("line_start".to_owned(), json!(symbol.line_start));
    attributes.insert("line_end".to_owned(), json!(symbol.line_end));
    if let Some(namespace) = &symbol.namespace {
        attributes.insert("namespace".to_owned(), json!(namespace));
    }
    if let Some(signature) = &symbol.signature {
        attributes.insert("signature".to_owned(), json!(signature));
    }
    EntityObservation {
        snapshot: revision.snapshot(),
        entity: Entity {
            id: EntityId::new(&symbol.id),
            model: model_id(),
            kind: symbol.kind.as_str().to_owned(),
            label: symbol.name.clone(),
        },
        attributes,
        measurements: Vec::new(),
        scores: Vec::new(),
    }
}

fn file_entity(revision: &RevisionId, path: &str, language: &str) -> EntityObservation {
    let mut attributes = Attributes::new();
    attributes.insert("language".to_owned(), json!(language));
    attributes.insert("file_path".to_owned(), json!(path));
    EntityObservation {
        snapshot: revision.snapshot(),
        entity: Entity {
            id: EntityId::new(format!("file:{path}")),
            model: model_id(),
            kind: NodeKind::File.as_str().to_owned(),
            label: path.to_owned(),
        },
        attributes,
        measurements: Vec::new(),
        scores: Vec::new(),
    }
}

fn contains_relation(revision: &RevisionId, path: &str, symbol: &str) -> RelationObservation {
    RelationObservation {
        snapshot: revision.snapshot(),
        relation: Relation {
            id: RelationId::new(format!("contains:{path}:{symbol}")),
            model: model_id(),
            kind: DependencyType::Contains.as_str(),
            from: EntityId::new(format!("file:{path}")),
            to: EntityId::new(symbol),
        },
        weight: DependencyType::Contains.weight(),
        attributes: Attributes::new(),
    }
}

fn empty_batch(
    snapshot: &detamu_core::SnapshotId,
    observer: &str,
    message: &str,
) -> ObservationBatch {
    let mut batch = ObservationBatch::empty(snapshot.clone());
    batch.coverage = AnalysisCoverage::Partial;
    batch
        .diagnostics
        .push(diagnostic(observer, None, message.to_owned()));
    batch
}

fn diagnostic(observer: &str, scope: Option<String>, message: String) -> AnalysisDiagnostic {
    AnalysisDiagnostic {
        severity: DiagnosticSeverity::Warning,
        observer: observer.to_owned(),
        message,
        scope,
    }
}

fn model_id() -> detamu_core::ModelId {
    detamu_core::ModelId::new(CODE_MODEL_ID)
}

fn token(value: &str, name: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        return Err(format!("{name} must be a non-empty token"));
    }
    Ok(value.to_owned())
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

struct MaterializedTree {
    root: PathBuf,
}

impl MaterializedTree {
    async fn create(id: &str) -> Result<Self, AnalyzerError> {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "detamu-lsp-{}-{}-{sequence}",
            sanitize(id),
            std::process::id()
        ));
        tokio::fs::create_dir_all(&root).await.map_err(|error| {
            AnalyzerError::Failed(format!("create language server workspace: {error}"))
        })?;
        Ok(Self { root })
    }

    async fn write_contents(&self, contents: Vec<ArtifactContent>) -> Result<(), AnalyzerError> {
        for content in contents {
            let relative = safe_relative_path(&content.artifact.path)?;
            let target = self.root.join(relative);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|error| {
                    AnalyzerError::Failed(format!("create artifact directory: {error}"))
                })?;
            }
            tokio::fs::write(target, content.bytes)
                .await
                .map_err(|error| AnalyzerError::Failed(format!("write artifact: {error}")))?;
        }
        Ok(())
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for MaterializedTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
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
    path.strip_prefix(workspace)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            AnalyzerError::Failed(format!(
                "path escaped language server workspace: {}",
                path.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use detamu_core::{SnapshotVersion, WorldId};

    use super::*;

    #[test]
    fn registration_normalizes_language_and_extensions() {
        let registration = LspRegistration {
            id: " pyright ".to_owned(),
            language: " Python ".to_owned(),
            command: " pyright-langserver ".to_owned(),
            arguments: vec!["--stdio".to_owned()],
            extensions: vec![".PY".to_owned(), "py".to_owned()],
            initialization_options: None,
        }
        .prepare()
        .expect("prepare registration");

        assert_eq!(registration.id, "pyright");
        assert_eq!(registration.language, "python");
        assert_eq!(registration.command, "pyright-langserver");
        assert_eq!(registration.extensions, ["py"]);
        assert!(registration.matches_path("src/app.py"));
    }

    #[test]
    fn csharp_document_symbols_emit_weighted_inheritance() {
        let revision = RevisionId::new(RepositoryId::new("repo"), GitOid::new("abc"));
        let symbols = json!([{
            "name": "Widget",
            "kind": 5,
            "detail": "class Widget : Base, IWidget",
            "range": {"start": {"line": 2, "character": 0}, "end": {"line": 10, "character": 1}},
            "selectionRange": {"start": {"line": 2, "character": 6}, "end": {"line": 2, "character": 12}},
            "children": [{
                "name": "Draw",
                "kind": 6,
                "detail": "void Draw()",
                "range": {"start": {"line": 4, "character": 4}, "end": {"line": 6, "character": 5}},
                "selectionRange": {"start": {"line": 4, "character": 9}, "end": {"line": 4, "character": 13}}
            }]
        }]);
        let mut catalog = Vec::new();
        collect_symbols(
            &symbols,
            "src/Widget.cs",
            "file:///src/Widget.cs",
            &mut catalog,
        );
        let mut edges = BTreeSet::new();
        let mut unresolved = BTreeSet::new();
        for symbol in &catalog {
            collect_bases(symbol, &catalog, "csharp", &mut edges, &mut unresolved);
        }
        let batch = assemble(
            &BatchContext {
                revision: &revision,
                language: "csharp",
                observer: "lsp.csharp",
                registration_id: "csharp",
            },
            &catalog,
            &edges,
            &unresolved,
            Vec::new(),
        );

        let widget = acc_symbol_id("src/Widget.cs", "Widget", 3);
        let draw = acc_symbol_id("src/Widget.cs", "Draw", 5);
        assert!(
            batch
                .entities
                .iter()
                .any(|entity| entity.entity.id.as_str() == widget.as_str()
                    && entity.entity.kind == "type")
        );
        assert!(
            batch
                .entities
                .iter()
                .any(|entity| entity.entity.id.as_str() == draw.as_str()
                    && entity.entity.kind == "method")
        );
        let inherits = batch
            .relations
            .iter()
            .find(|relation| relation.relation.kind == "inherits")
            .expect("inherits edge");
        let implements = batch
            .relations
            .iter()
            .find(|relation| relation.relation.kind == "implements")
            .expect("implements edge");
        assert_eq!(inherits.weight, 1.0);
        assert_eq!(implements.weight, 1.0);
        assert_eq!(inherits.relation.from.as_str(), widget.as_str());
        assert_eq!(batch.provenance[0].source.as_deref(), Some("csharp"));
        assert!(batch.entities.iter().any(|entity| {
            entity.attributes.get("resolution").and_then(Value::as_str) == Some("unresolved")
        }));
    }

    #[test]
    fn call_edges_use_the_acc_weight() {
        let revision = RevisionId::new(RepositoryId::new("repo"), GitOid::new("abc"));
        let source = symbol("src/app.py", "caller", 1);
        let target = symbol("src/app.py", "callee", 8);
        let mut edges = BTreeSet::new();
        edges.insert((DependencyType::Calls, source.id.clone(), target.id.clone()));
        let batch = assemble(
            &BatchContext {
                revision: &revision,
                language: "python",
                observer: "lsp.python",
                registration_id: "python",
            },
            &[source, target],
            &edges,
            &BTreeSet::new(),
            Vec::new(),
        );
        let call = batch
            .relations
            .iter()
            .find(|relation| relation.relation.kind == "calls")
            .expect("call edge");
        assert_eq!(call.weight, 0.7);
    }

    fn symbol(path: &str, name: &str, line: u32) -> CatalogSymbol {
        CatalogSymbol {
            id: acc_symbol_id(path, name, line).as_str().to_owned(),
            name: name.to_owned(),
            kind: NodeKind::Function,
            path: path.to_owned(),
            line_start: line,
            line_end: line,
            namespace: None,
            signature: None,
            callable: true,
            selection_start: json!({"line": line - 1, "character": 0}),
            range: json!({"start": {"line": line - 1, "character": 0}, "end": {"line": line - 1, "character": 1}}),
            uri: format!("file:///{path}"),
            detail: None,
        }
    }

    #[test]
    fn snapshot_round_trip_uses_the_code_world_prefix() {
        let snapshot = detamu_core::SnapshotId::new(
            WorldId::new("code.repository:fixture"),
            SnapshotVersion::new("abc"),
        );
        let input = AnalysisInput {
            snapshot,
            sources: Vec::new(),
            changed_entities: None,
        };
        assert_eq!(revision(&input).expect("revision").commit.as_str(), "abc");
    }
}
