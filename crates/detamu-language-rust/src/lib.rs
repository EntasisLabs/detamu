//! Rust syntax analyzer for the Detamu code world model.

use std::{collections::BTreeSet, path::Path, sync::Arc};

use detamu_core::ObservationBatch;
use detamu_language::LanguagePack;
use detamu_language_tree_sitter::{TreeSitterAnalyzer, TreeSitterSpec};
use detamu_model::{AnalyzerCapability, Artifact, ArtifactReader, ModelAnalyzer};
use detamu_model_code::{
    CodeSymbol, FileHistory, LanguageId, NodeKind, RecentFrequency, RevisionId, SymbolLocation,
    SyntaxMetrics, acc_symbol_id, file_contains_symbol, file_imports_module,
    imported_module_observation, syntax_symbol_observation,
};
use tree_sitter::Node;

pub type RustLanguageAnalyzer = TreeSitterAnalyzer<RustTreeSitterSpec>;

#[derive(Debug, Clone, Copy, Default)]
pub struct RustTreeSitterSpec;

pub struct RustLanguagePack {
    artifacts: Arc<dyn ArtifactReader>,
}

impl RustLanguagePack {
    pub fn new(artifacts: Arc<dyn ArtifactReader>) -> Self {
        Self { artifacts }
    }
}

impl LanguagePack for RustLanguagePack {
    fn language(&self) -> LanguageId {
        LanguageId::new("rust")
    }

    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn analyzers(&self) -> Vec<Arc<dyn ModelAnalyzer>> {
        vec![Arc::new(TreeSitterAnalyzer::new(
            self.artifacts.clone(),
            RustTreeSitterSpec,
        ))]
    }
}

impl TreeSitterSpec for RustTreeSitterSpec {
    fn language(&self) -> LanguageId {
        LanguageId::new("rust")
    }

    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn grammar(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn observer(&self) -> &'static str {
        "treesitter.rust"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn configuration_digest(&self) -> &'static str {
        "syntax-metrics-v1"
    }

    fn capabilities(&self) -> Vec<AnalyzerCapability> {
        vec![
            AnalyzerCapability::Symbols,
            AnalyzerCapability::Hierarchy,
            AnalyzerCapability::Metrics,
            AnalyzerCapability::Imports,
        ]
    }

    fn observe_tree(
        &self,
        revision: &RevisionId,
        artifact: &Artifact,
        source: &[u8],
        root: Node<'_>,
        batch: &mut ObservationBatch,
    ) {
        let history = history(artifact);
        collect_imports(revision, root, source, &artifact.path, batch);
        collect_symbols(
            revision,
            root,
            source,
            &artifact.path,
            history.as_ref(),
            batch,
        );
    }
}

fn collect_imports(
    revision: &RevisionId,
    root: Node<'_>,
    source: &[u8],
    path: &str,
    batch: &mut ObservationBatch,
) {
    let mut imports = BTreeSet::new();
    collect_import_paths(root, source, &mut imports);
    let language = LanguageId::new("rust");
    for import_path in imports {
        batch.entities.push(imported_module_observation(
            revision,
            &language,
            &import_path,
        ));
        batch
            .relations
            .push(file_imports_module(revision, path, &language, &import_path));
    }
}

fn collect_import_paths(node: Node<'_>, source: &[u8], imports: &mut BTreeSet<String>) {
    if node.kind() == "use_declaration"
        && let Ok(text) = node.utf8_text(source)
    {
        let import = text
            .trim()
            .strip_prefix("use ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .trim();
        if !import.is_empty() {
            imports.insert(import.to_owned());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_import_paths(child, source, imports);
    }
}

fn collect_symbols(
    revision: &RevisionId,
    node: Node<'_>,
    source: &[u8],
    path: &str,
    history: Option<&FileHistory>,
    batch: &mut ObservationBatch,
) {
    if let Some(kind) = symbol_kind(node)
        && let Some(name_node) = node.child_by_field_name("name")
        && let Ok(name) = name_node.utf8_text(source)
    {
        let line_start = u32::try_from(node.start_position().row + 1).unwrap_or(u32::MAX);
        let line_end = u32::try_from(node.end_position().row + 1).unwrap_or(u32::MAX);
        let qualified_name = qualified_name(node, source, path, name);
        let symbol_id = acc_symbol_id(path, name, line_start);
        let signature = signature(node, source);
        let metrics = SyntaxMetrics {
            lines_of_code: line_end.saturating_sub(line_start).saturating_add(1),
            cyclomatic_complexity: complexity(node, source),
            parameters: parameter_count(node),
        };
        batch.entities.push(syntax_symbol_observation(
            revision,
            CodeSymbol {
                id: symbol_id.clone(),
                language: LanguageId::new("rust"),
                qualified_name,
                kind,
            },
            SymbolLocation {
                file_path: path,
                line_start,
                line_end,
                signature: signature.as_deref(),
            },
            metrics,
            history,
            "treesitter.rust",
            0.9,
        ));
        batch
            .relations
            .push(file_contains_symbol(revision, path, &symbol_id));
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbols(revision, child, source, path, history, batch);
    }
}

fn symbol_kind(node: Node<'_>) -> Option<NodeKind> {
    match node.kind() {
        "function_item" => Some(if has_ancestor(node, &["impl_item", "trait_item"]) {
            NodeKind::Method
        } else {
            NodeKind::Function
        }),
        "function_signature_item" => Some(NodeKind::Method),
        "struct_item" | "enum_item" | "union_item" | "type_item" => Some(NodeKind::Type),
        "trait_item" => Some(NodeKind::Trait),
        "mod_item" => Some(NodeKind::Module),
        _ => None,
    }
}

fn has_ancestor(mut node: Node<'_>, kinds: &[&str]) -> bool {
    while let Some(parent) = node.parent() {
        if kinds.contains(&parent.kind()) {
            return true;
        }
        node = parent;
    }
    false
}

fn qualified_name(node: Node<'_>, source: &[u8], path: &str, name: &str) -> String {
    let mut parts = module_parts(path);
    let mut ancestors = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "mod_item" | "trait_item" | "function_item" => {
                if let Some(value) = field_text(parent, "name", source) {
                    ancestors.push(value);
                }
            }
            "impl_item" => {
                if let Some(value) = field_text(parent, "type", source) {
                    ancestors.push(value);
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    ancestors.reverse();
    parts.extend(ancestors);
    parts.push(name.to_owned());
    parts.join("::")
}

fn module_parts(path: &str) -> Vec<String> {
    let path = Path::new(path);
    let mut parts = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|part| part == "src") {
        parts.remove(0);
    }
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned());
    if let Some(stem) = stem.filter(|stem| !matches!(stem.as_str(), "lib" | "main" | "mod")) {
        parts.push(stem);
    }
    parts
}

fn field_text(node: Node<'_>, field: &str, source: &[u8]) -> Option<String> {
    node.child_by_field_name(field)?
        .utf8_text(source)
        .ok()
        .map(str::to_owned)
}

fn signature(node: Node<'_>, source: &[u8]) -> Option<String> {
    let end = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    std::str::from_utf8(&source[node.start_byte()..end])
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn parameter_count(node: Node<'_>) -> u32 {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return 0;
    };
    u32::try_from(parameters.named_child_count()).unwrap_or(u32::MAX)
}

fn complexity(node: Node<'_>, source: &[u8]) -> u32 {
    if !matches!(node.kind(), "function_item" | "function_signature_item") {
        return 0;
    }
    let mut value = 1_u32;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        value = value.saturating_add(complexity_increment(child, source));
    }
    value
}

fn complexity_increment(node: Node<'_>, source: &[u8]) -> u32 {
    let own = match node.kind() {
        "if_expression" | "while_expression" | "for_expression" | "loop_expression"
        | "match_arm" | "try_expression" => 1,
        "binary_expression" => node.utf8_text(source).ok().map_or(0, |text| {
            u32::from(text.contains("&&") || text.contains("||"))
        }),
        _ => 0,
    };
    let mut value = own;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        value = value.saturating_add(complexity_increment(child, source));
    }
    value
}

fn history(artifact: &Artifact) -> Option<FileHistory> {
    Some(FileHistory {
        created_at: string_attribute(artifact, "git.created_at")?,
        last_modified_at: string_attribute(artifact, "git.last_modified_at")?,
        total_commits: u32_attribute(artifact, "git.total_commits")?,
        contributors: u32_attribute(artifact, "git.contributors")?,
        average_days_between_changes: artifact
            .attributes
            .get("git.average_days_between_changes")?
            .as_f64()?,
        recent_commits: u32_attribute(artifact, "git.recent_commits")?,
        recent_frequency: match string_attribute(artifact, "git.recent_frequency")?.as_str() {
            "low" => RecentFrequency::Low,
            "medium" => RecentFrequency::Medium,
            "high" => RecentFrequency::High,
            _ => return None,
        },
    })
}

fn string_attribute(artifact: &Artifact, name: &str) -> Option<String> {
    artifact.attributes.get(name)?.as_str().map(str::to_owned)
}

fn u32_attribute(artifact: &Artifact, name: &str) -> Option<u32> {
    u32::try_from(artifact.attributes.get(name)?.as_u64()?).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use detamu_core::{SnapshotId, SnapshotVersion, WorldId};
    use detamu_model::{AnalysisInput, ArtifactContent, ArtifactError, SourceReference};
    use serde_json::json;

    use super::*;

    const SOURCE: &str = r"
use std::fmt;

pub struct Greeter;

impl Greeter {
    pub fn greet(&self, ready: bool) -> u8 {
        if ready && true { 1 } else { 0 }
    }
}

pub fn choose(value: u8) -> u8 {
    match value {
        0 => 1,
        _ => 2,
    }
}
";

    #[derive(Clone)]
    struct FixtureReader {
        artifact: Artifact,
    }

    #[async_trait]
    impl ArtifactReader for FixtureReader {
        fn supports(&self, source: &SourceReference) -> bool {
            source.kind == "fixture"
        }

        async fn artifacts(
            &self,
            _source: &SourceReference,
        ) -> Result<Vec<Artifact>, ArtifactError> {
            Ok(vec![self.artifact.clone()])
        }

        async fn read_many(
            &self,
            _source: &SourceReference,
            artifacts: &[Artifact],
        ) -> Result<Vec<ArtifactContent>, ArtifactError> {
            Ok(artifacts
                .iter()
                .cloned()
                .map(|artifact| ArtifactContent {
                    artifact,
                    bytes: SOURCE.as_bytes().to_vec(),
                })
                .collect())
        }
    }

    fn artifact() -> Artifact {
        let mut attributes = BTreeMap::new();
        attributes.insert("language".to_owned(), json!("rust"));
        attributes.insert("git.created_at".to_owned(), json!("2024-01-01T00:00:00Z"));
        attributes.insert(
            "git.last_modified_at".to_owned(),
            json!("2024-01-11T00:00:00Z"),
        );
        attributes.insert("git.total_commits".to_owned(), json!(2));
        attributes.insert("git.contributors".to_owned(), json!(1));
        attributes.insert("git.average_days_between_changes".to_owned(), json!(10.0));
        attributes.insert("git.recent_commits".to_owned(), json!(2));
        attributes.insert("git.recent_frequency".to_owned(), json!("low"));
        Artifact {
            path: "src/lib.rs".to_owned(),
            content_id: "fixture".to_owned(),
            media_type: Some("text/x-rust".to_owned()),
            attributes,
        }
    }

    #[tokio::test]
    async fn emits_symbols_metrics_and_file_containment() {
        let analyzer = RustLanguageAnalyzer::new(
            Arc::new(FixtureReader {
                artifact: artifact(),
            }),
            RustTreeSitterSpec,
        );
        let snapshot = SnapshotId::new(
            WorldId::new("code.repository:fixture"),
            SnapshotVersion::new("abc123"),
        );
        let batch = analyzer
            .analyze(&AnalysisInput {
                snapshot: snapshot.clone(),
                sources: vec![SourceReference {
                    kind: "fixture".to_owned(),
                    locator: "fixture".to_owned(),
                    cursor: Some("abc123".to_owned()),
                    attributes: BTreeMap::new(),
                }],
                changed_entities: None,
            })
            .await
            .expect("analyze Rust fixture");

        assert_eq!(batch.snapshot, snapshot);
        assert_eq!(batch.entities.len(), 4);
        assert_eq!(batch.relations.len(), 4);
        let greet = batch
            .entities
            .iter()
            .find(|entity| entity.entity.label == "Greeter::greet")
            .expect("greet method");
        assert_eq!(greet.entity.kind, "method");
        assert_eq!(measurement(greet, "code.parameters"), Some(2.0));
        assert_eq!(measurement(greet, "code.cyclomatic_complexity"), Some(3.0));
        assert_eq!(measurement(greet, "git.total_commits"), Some(2.0));
        assert_eq!(
            batch
                .relations
                .iter()
                .filter(|relation| relation.relation.kind == "contains")
                .count(),
            3
        );
        assert!(batch.relations.iter().any(|relation| {
            relation.relation.kind == "imports"
                && relation.relation.from.as_str() == "file:src/lib.rs"
        }));
        assert!(
            batch
                .entities
                .iter()
                .any(|entity| entity.entity.label == "std::fmt")
        );
    }

    fn measurement(entity: &detamu_core::EntityObservation, name: &str) -> Option<f64> {
        entity
            .measurements
            .iter()
            .find(|measurement| measurement.name == name)
            .map(|measurement| measurement.value)
    }
}
