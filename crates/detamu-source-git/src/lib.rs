//! Deterministic Git source adapter for the Detamu code world model.
//!
//! Repository snapshots are identified by commit OID. Branch and working-tree
//! state are metadata only. File inventory is read from the commit tree, so it
//! remains stable even when the working tree is dirty.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Output,
};

use async_trait::async_trait;
use detamu_core::{AnalysisCoverage, ModelId, ObservationBatch, ObserverProvenance};
use detamu_model::{
    AnalysisInput, AnalyzerDescriptor, AnalyzerError, ModelAnalyzer, SourceDescriptor, SourceError,
    SourceReference, SourceRequest, SourceResolution, WorldSource,
};
use detamu_model_code::{
    CODE_MODEL_ID, GitOid, LanguageId, RepositoryId, RevisionId, file_observation,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::process::Command;

pub const GIT_SOURCE_KIND: &str = "git_repository";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub root: PathBuf,
    pub repository: RepositoryId,
    pub commit: GitOid,
    pub branch: Option<String>,
    pub remote: Option<String>,
    pub dirty: bool,
}

impl RepositorySnapshot {
    pub fn revision(&self) -> RevisionId {
        RevisionId::new(self.repository.clone(), self.commit.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedFile {
    pub path: String,
    pub blob_oid: String,
    pub mode: String,
    pub size: Option<u64>,
    pub language: LanguageId,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitRepositorySource;

impl GitRepositorySource {
    /// Resolves a path inside a Git worktree to an immutable repository snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when Git is unavailable, the path is not in a repository,
    /// or the requested revision does not resolve to a commit.
    pub async fn inspect(
        path: impl AsRef<Path>,
        requested_version: Option<&str>,
    ) -> Result<RepositorySnapshot, SourceError> {
        let requested_path = path.as_ref();
        let root_output = git(requested_path, &["rev-parse", "--show-toplevel"]).await?;
        let root = PathBuf::from(text(root_output, "repository root")?);
        let root = tokio::fs::canonicalize(&root).await.map_err(|error| {
            SourceError::Failed(format!("canonicalize repository root: {error}"))
        })?;

        let revision = requested_version.unwrap_or("HEAD");
        let commit_expression = format!("{revision}^{{commit}}");
        let commit = text(
            git(&root, &["rev-parse", "--verify", &commit_expression]).await?,
            "commit OID",
        )?;
        let remote = optional_git(&root, &["config", "--get", "remote.origin.url"])
            .await?
            .map(|output| text(output, "origin URL"))
            .transpose()?;
        let branch = optional_git(&root, &["symbolic-ref", "--short", "-q", "HEAD"])
            .await?
            .map(|output| text(output, "branch name"))
            .transpose()?;
        let dirty = !git(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        .await?
        .stdout
        .is_empty();
        let repository = repository_id(&root, remote.as_deref());

        Ok(RepositorySnapshot {
            root,
            repository,
            commit: GitOid::new(commit),
            branch,
            remote: remote.and_then(|value| normalize_remote(&value)),
            dirty,
        })
    }

    /// Lists supported tracked source files from the snapshot's commit tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the Git tree cannot be read or contains a non-UTF-8
    /// path.
    pub async fn tracked_files(
        snapshot: &RepositorySnapshot,
    ) -> Result<Vec<TrackedFile>, SourceError> {
        let output = git(
            &snapshot.root,
            &[
                "ls-tree",
                "-r",
                "-z",
                "-l",
                "--full-tree",
                snapshot.commit.as_str(),
            ],
        )
        .await?;
        parse_tree(&output.stdout)
    }
}

#[async_trait]
impl WorldSource for GitRepositorySource {
    fn descriptor(&self) -> SourceDescriptor {
        SourceDescriptor {
            name: "git".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            model: ModelId::new(CODE_MODEL_ID),
        }
    }

    async fn resolve(&self, request: &SourceRequest) -> Result<SourceResolution, SourceError> {
        let snapshot = Self::inspect(&request.locator, request.version.as_deref()).await?;
        let revision = snapshot.revision();
        let mut attributes = BTreeMap::new();
        attributes.insert(
            "repository_id".to_owned(),
            json!(snapshot.repository.as_str()),
        );
        attributes.insert("branch".to_owned(), json!(snapshot.branch));
        attributes.insert("remote".to_owned(), json!(snapshot.remote));
        attributes.insert("working_tree_dirty".to_owned(), json!(snapshot.dirty));

        let reference = SourceReference {
            kind: GIT_SOURCE_KIND.to_owned(),
            locator: snapshot.root.to_string_lossy().into_owned(),
            cursor: Some(snapshot.commit.as_str().to_owned()),
            attributes: attributes.clone(),
        };
        Ok(SourceResolution {
            input: AnalysisInput {
                snapshot: revision.snapshot(),
                sources: vec![reference],
                changed_entities: None,
            },
            metadata: attributes,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitRepositoryAnalyzer;

#[async_trait]
impl ModelAnalyzer for GitRepositoryAnalyzer {
    fn descriptor(&self) -> AnalyzerDescriptor {
        AnalyzerDescriptor {
            name: "git.repository.inventory".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            model: ModelId::new(CODE_MODEL_ID),
            capabilities: vec!["tracked_files".to_owned(), "language_detection".to_owned()],
        }
    }

    async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
        let source = input
            .sources
            .iter()
            .find(|source| source.kind == GIT_SOURCE_KIND)
            .ok_or_else(|| {
                AnalyzerError::Unavailable("Git repository source is missing".to_owned())
            })?;
        let commit = source
            .cursor
            .as_deref()
            .ok_or_else(|| AnalyzerError::Failed("Git source cursor is missing".to_owned()))?;
        let snapshot = GitRepositorySource::inspect(&source.locator, Some(commit))
            .await
            .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
        if snapshot.revision().snapshot() != input.snapshot {
            return Err(AnalyzerError::Failed(
                "Git source resolved to a different snapshot".to_owned(),
            ));
        }
        let files = GitRepositorySource::tracked_files(&snapshot)
            .await
            .map_err(|error| AnalyzerError::Failed(error.to_string()))?;
        let revision = snapshot.revision();
        let mut batch = ObservationBatch::empty(input.snapshot.clone());
        batch.coverage = AnalysisCoverage::Partial;
        batch.provenance.push(ObserverProvenance {
            observer: "git.repository.inventory".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            configuration_digest: None,
            source: Some(snapshot.root.to_string_lossy().into_owned()),
        });
        batch.entities = files
            .iter()
            .map(|file| {
                file_observation(
                    &revision,
                    &file.path,
                    &file.blob_oid,
                    &file.mode,
                    file.size,
                    &file.language,
                )
            })
            .collect();
        Ok(batch)
    }
}

fn repository_id(root: &Path, remote: Option<&str>) -> RepositoryId {
    if let Some(normalized) = remote.and_then(normalize_remote) {
        return RepositoryId::new(format!("remote:{normalized}"));
    }
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    RepositoryId::new(format!("local:{}", &digest[..32]))
}

fn normalize_remote(remote: &str) -> Option<String> {
    let mut value = remote.trim().trim_end_matches('/').to_owned();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix("git@") {
        value = rest.replacen(':', "/", 1);
    } else if let Some((_, rest)) = value.split_once("://") {
        value = rest.to_owned();
        if let Some((_, without_user)) = value.split_once('@') {
            value = without_user.to_owned();
        }
    }
    Some(value.trim_end_matches(".git").to_owned())
}

fn parse_tree(bytes: &[u8]) -> Result<Vec<TrackedFile>, SourceError> {
    let mut files = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| SourceError::Failed("malformed Git tree record".to_owned()))?;
        let (metadata, path_with_separator) = record.split_at(separator);
        let path = &path_with_separator[1..];
        let path = std::str::from_utf8(path)
            .map_err(|_| SourceError::Failed("Git tree contains a non-UTF-8 path".to_owned()))?;
        let Some(language) = detect_language(path) else {
            continue;
        };
        let metadata = std::str::from_utf8(metadata)
            .map_err(|_| SourceError::Failed("malformed Git tree metadata".to_owned()))?;
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 || fields[1] != "blob" {
            continue;
        }
        files.push(TrackedFile {
            path: path.to_owned(),
            blob_oid: fields[2].to_owned(),
            mode: fields[0].to_owned(),
            size: (fields[3] != "-")
                .then(|| fields[3].parse::<u64>())
                .transpose()
                .map_err(|error| SourceError::Failed(format!("invalid Git blob size: {error}")))?,
            language,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

pub fn detect_language(path: &str) -> Option<LanguageId> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    let language = match extension.as_str() {
        "cs" => "csharp",
        "ts" => "typescript",
        "js" => "javascript",
        "py" => "python",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "cpp" => "cpp",
        "c" | "h" => "c",
        _ => return None,
    };
    Some(LanguageId::new(language))
}

async fn git(path: &Path, arguments: &[&str]) -> Result<Output, SourceError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .output()
        .await
        .map_err(|error| SourceError::Unavailable(format!("run Git: {error}")))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(SourceError::Failed(format!(
            "git {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

async fn optional_git(path: &Path, arguments: &[&str]) -> Result<Option<Output>, SourceError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .output()
        .await
        .map_err(|error| SourceError::Unavailable(format!("run Git: {error}")))?;
    if output.status.success() {
        Ok(Some(output))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(SourceError::Failed(format!(
            "git {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn text(output: Output, field: &str) -> Result<String, SourceError> {
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| SourceError::Failed(format!("Git returned a non-UTF-8 {field}")))
}
