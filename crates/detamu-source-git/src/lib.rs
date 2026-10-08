//! Deterministic Git source adapter for the Detamu code world model.
//!
//! Repository snapshots are identified by commit OID. Branch and working-tree
//! state are metadata only. File inventory is read from the commit tree, so it
//! remains stable even when the working tree is dirty.

mod history;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use detamu_core::{AnalysisCoverage, ModelId, ObservationBatch, ObserverProvenance};
use detamu_model::{
    AnalysisInput, AnalyzerCapability, AnalyzerDescriptor, AnalyzerError, AnalyzerExecution,
    Artifact, ArtifactContent, ArtifactError, ArtifactReader, ModelAnalyzer, SourceDescriptor,
    SourceError, SourceReference, SourceRequest, SourceResolution, WorldSource,
};
use detamu_model_code::{
    CODE_MODEL_ID, FileHistory, GitOid, LanguageId, RepositoryId, RevisionId, file_observation,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    pub language: Option<LanguageId>,
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

    /// Lists every tracked blob from the snapshot's commit tree.
    ///
    /// # Errors
    ///
    /// Returns an error when the Git tree cannot be read or contains a non-UTF-8
    /// path.
    pub async fn tracked_files(
        snapshot: &RepositorySnapshot,
    ) -> Result<Vec<TrackedFile>, SourceError> {
        Ok(cached_inventory(snapshot).await?.files.as_ref().clone())
    }
}

struct RepositoryInventory {
    files: Arc<Vec<TrackedFile>>,
    histories: Arc<HashMap<String, FileHistory>>,
}

struct InventoryKey {
    root: PathBuf,
    commit: String,
}

fn inventory_slot() -> &'static Mutex<Option<(InventoryKey, Arc<RepositoryInventory>)>> {
    static CACHE: Mutex<Option<(InventoryKey, Arc<RepositoryInventory>)>> = Mutex::new(None);
    &CACHE
}

async fn cached_inventory(
    snapshot: &RepositorySnapshot,
) -> Result<Arc<RepositoryInventory>, SourceError> {
    let commit = snapshot.commit.as_str().to_owned();
    {
        let cache = inventory_slot()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((key, inventory)) = cache.as_ref()
            && key.root == snapshot.root
            && key.commit == commit
        {
            return Ok(Arc::clone(inventory));
        }
    }

    let inventory = Arc::new(RepositoryInventory {
        files: Arc::new(read_tracked_files(snapshot).await?),
        histories: Arc::new(history::load_file_histories(snapshot).await?),
    });
    let mut cache = inventory_slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache = Some((
        InventoryKey {
            root: snapshot.root.clone(),
            commit,
        },
        Arc::clone(&inventory),
    ));
    Ok(inventory)
}

async fn read_tracked_files(
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

#[async_trait]
impl ArtifactReader for GitRepositorySource {
    fn supports(&self, source: &SourceReference) -> bool {
        source.kind == GIT_SOURCE_KIND && source.cursor.is_some()
    }

    async fn artifacts(&self, source: &SourceReference) -> Result<Vec<Artifact>, ArtifactError> {
        let commit = source
            .cursor
            .as_deref()
            .ok_or_else(|| ArtifactError::Failed("Git source cursor is missing".to_owned()))?;
        let snapshot = Self::inspect(&source.locator, Some(commit))
            .await
            .map_err(|error| ArtifactError::Failed(error.to_string()))?;
        let inventory = cached_inventory(&snapshot)
            .await
            .map_err(|error| ArtifactError::Failed(error.to_string()))?;
        Ok(inventory
            .files
            .iter()
            .map(|file| {
                let history = file
                    .language
                    .as_ref()
                    .and_then(|_| inventory.histories.get(&file.path));
                artifact(file, history)
            })
            .collect())
    }

    async fn read_many(
        &self,
        source: &SourceReference,
        artifacts: &[Artifact],
    ) -> Result<Vec<ArtifactContent>, ArtifactError> {
        let commit = source
            .cursor
            .as_deref()
            .ok_or_else(|| ArtifactError::Failed("Git source cursor is missing".to_owned()))?;
        if artifacts.is_empty() {
            return Ok(Vec::new());
        }
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&source.locator)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| ArtifactError::Unavailable(format!("run Git: {error}")))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ArtifactError::Failed("Git stdin is unavailable".to_owned()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| ArtifactError::Failed("Git stdout is unavailable".to_owned()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| ArtifactError::Failed("Git stderr is unavailable".to_owned()))?;
        let requests = artifacts
            .iter()
            .map(|artifact| {
                if artifact.path.contains(['\n', '\r']) {
                    return Err(ArtifactError::Failed(
                        "Git batch reader does not support newline-containing paths".to_owned(),
                    ));
                }
                Ok(format!("{commit}:{}\n", artifact.path))
            })
            .collect::<Result<String, _>>()?;
        let writer = tokio::spawn(async move {
            stdin.write_all(requests.as_bytes()).await?;
            stdin.shutdown().await
        });
        let stderr_task = tokio::spawn(async move {
            let mut stderr_bytes = Vec::new();
            let _ = stderr.read_to_end(&mut stderr_bytes).await;
            stderr_bytes
        });
        let read = read_batch_objects(&mut stdout, artifacts).await;
        let write = writer
            .await
            .map_err(|error| ArtifactError::Failed(format!("write Git requests: {error}")))?;
        let status = child
            .wait()
            .await
            .map_err(|error| ArtifactError::Failed(format!("wait for Git: {error}")))?;
        let stderr_bytes = stderr_task.await.unwrap_or_default();
        if !status.success() {
            return Err(ArtifactError::Failed(format!(
                "git cat-file: {}",
                String::from_utf8_lossy(&stderr_bytes).trim()
            )));
        }
        write.map_err(|error| ArtifactError::Failed(format!("write Git requests: {error}")))?;
        read
    }
}

async fn read_batch_objects(
    stdout: &mut tokio::process::ChildStdout,
    artifacts: &[Artifact],
) -> Result<Vec<ArtifactContent>, ArtifactError> {
    let mut stdout = BufReader::new(stdout);
    let mut contents = Vec::with_capacity(artifacts.len());
    let mut header = Vec::new();
    for artifact in artifacts {
        header.clear();
        let read = stdout
            .read_until(b'\n', &mut header)
            .await
            .map_err(|error| ArtifactError::Failed(format!("read Git object header: {error}")))?;
        if read == 0 || header.last() != Some(&b'\n') {
            return Err(ArtifactError::Failed(
                "truncated Git object header".to_owned(),
            ));
        }
        header.pop();
        let header = std::str::from_utf8(&header)
            .map_err(|_| ArtifactError::Failed("non-UTF-8 Git object header".to_owned()))?;
        let mut fields = header.split_whitespace();
        let object_id = fields
            .next()
            .ok_or_else(|| ArtifactError::Failed(format!("Git object is not a blob: {header}")))?;
        let object_type = fields
            .next()
            .ok_or_else(|| ArtifactError::Failed(format!("Git object is not a blob: {header}")))?;
        let size = fields
            .next()
            .ok_or_else(|| ArtifactError::Failed(format!("Git object is not a blob: {header}")))?;
        if object_type != "blob" || fields.next().is_some() {
            return Err(ArtifactError::Failed(format!(
                "Git object is not a blob: {header}"
            )));
        }
        if object_id != artifact.content_id {
            return Err(ArtifactError::Failed(format!(
                "Git returned {object_id} for expected blob {}",
                artifact.content_id
            )));
        }
        let size = size
            .parse::<usize>()
            .map_err(|error| ArtifactError::Failed(format!("invalid Git blob size: {error}")))?;
        let mut bytes = vec![0; size];
        stdout
            .read_exact(&mut bytes)
            .await
            .map_err(|error| ArtifactError::Failed(format!("truncated Git blob: {error}")))?;
        let mut newline = [0; 1];
        stdout.read_exact(&mut newline).await.map_err(|error| {
            ArtifactError::Failed(format!("malformed Git blob terminator: {error}"))
        })?;
        if newline[0] != b'\n' {
            return Err(ArtifactError::Failed(
                "malformed Git blob terminator".to_owned(),
            ));
        }
        contents.push(ArtifactContent {
            artifact: artifact.clone(),
            bytes,
        });
    }
    Ok(contents)
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
            capabilities: vec![
                AnalyzerCapability::Other("tracked_files".to_owned()),
                AnalyzerCapability::Other("language_detection".to_owned()),
                AnalyzerCapability::Metrics,
            ],
            execution: AnalyzerExecution::Required,
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
        let inventory = cached_inventory(&snapshot)
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
        batch.entities = inventory
            .files
            .iter()
            .filter_map(|file| {
                let language = file.language.as_ref()?;
                file_observation(
                    &revision,
                    &file.path,
                    &file.blob_oid,
                    &file.mode,
                    file.size,
                    language,
                    inventory.histories.get(&file.path),
                )
                .into()
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

fn artifact(file: &TrackedFile, history: Option<&FileHistory>) -> Artifact {
    let mut attributes = BTreeMap::new();
    if let Some(language) = &file.language {
        attributes.insert("language".to_owned(), json!(language.as_str()));
    }
    attributes.insert("git.mode".to_owned(), json!(file.mode));
    attributes.insert("file.size_bytes".to_owned(), json!(file.size));
    if let Some(history) = history {
        attributes.insert("git.created_at".to_owned(), json!(history.created_at));
        attributes.insert(
            "git.last_modified_at".to_owned(),
            json!(history.last_modified_at),
        );
        attributes.insert("git.total_commits".to_owned(), json!(history.total_commits));
        attributes.insert("git.contributors".to_owned(), json!(history.contributors));
        attributes.insert(
            "git.average_days_between_changes".to_owned(),
            json!(history.average_days_between_changes),
        );
        attributes.insert(
            "git.recent_commits".to_owned(),
            json!(history.recent_commits),
        );
        attributes.insert(
            "git.recent_frequency".to_owned(),
            json!(history.recent_frequency.as_str()),
        );
    }
    Artifact {
        path: file.path.clone(),
        content_id: file.blob_oid.clone(),
        media_type: file
            .language
            .as_ref()
            .and_then(media_type)
            .map(str::to_owned),
        attributes,
    }
}

fn media_type(language: &LanguageId) -> Option<&'static str> {
    match language.as_str() {
        "rust" => Some("text/x-rust"),
        "csharp" => Some("text/x-csharp"),
        "typescript" => Some("text/typescript"),
        "javascript" => Some("text/javascript"),
        "python" => Some("text/x-python"),
        "go" => Some("text/x-go"),
        "java" => Some("text/x-java-source"),
        "c" => Some("text/x-c"),
        "cpp" => Some("text/x-c++"),
        _ => None,
    }
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
        if path.contains(['\n', '\r']) {
            return Err(SourceError::Failed(
                "Git tree contains a newline in a path".to_owned(),
            ));
        }
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
            language: detect_language(path),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

pub fn detect_language(path: &str) -> Option<LanguageId> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    let language = match extension.as_str() {
        "cs" => "csharp",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "c" | "h" => "c",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "lua" => "lua",
        "pl" | "pm" => "perl",
        "sol" => "solidity",
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
