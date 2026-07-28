use std::{fs, path::Path, process::Command, sync::Arc};

use detamu_core::AnalysisCoverage;
use detamu_model::{ArtifactReader, ModelAnalyzer, SourceRequest, WorldSource};
use detamu_source_git::{GitRepositoryAnalyzer, GitRepositorySource};

fn run_git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_AUTHOR_NAME", "Detamu Test")
        .env("GIT_AUTHOR_EMAIL", "detamu@example.test")
        .env("GIT_COMMITTER_NAME", "Detamu Test")
        .env("GIT_COMMITTER_EMAIL", "detamu@example.test")
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {}: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary repository");
    let root = directory.path();
    run_git(root, &["init", "-q"]);
    run_git(root, &["checkout", "-q", "-b", "main"]);
    fs::create_dir_all(root.join("src/nested")).expect("create source directory");
    fs::create_dir_all(root.join("web")).expect("create web directory");
    fs::write(root.join("src/lib.rs"), "pub fn run() {}\n").expect("write Rust source");
    fs::write(root.join("web/app.ts"), "export const app = 1;\n").expect("write TS source");
    fs::write(root.join("README.md"), "# Fixture\n").expect("write readme");
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "fixture"]);
    run_git(
        root,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:EntasisLabs/fixture.git",
        ],
    );
    directory
}

fn commit_at(root: &Path, message: &str, date: &str, name: &str, email: &str) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("commit fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn head(root: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("resolve fixture head");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("UTF-8 OID")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn discovery_is_rooted_at_an_immutable_commit() {
    let directory = fixture();
    let nested = directory.path().join("src/nested");
    let initial = GitRepositorySource::inspect(&nested, None)
        .await
        .expect("inspect repository");
    assert_eq!(initial.root, directory.path().canonicalize().expect("root"));
    assert_eq!(
        initial.repository.as_str(),
        "remote:github.com/EntasisLabs/fixture"
    );
    assert_eq!(initial.branch.as_deref(), Some("main"));
    assert!(!initial.dirty);

    fs::write(directory.path().join("src/lib.rs"), "dirty\n").expect("dirty tracked file");
    fs::write(directory.path().join("src/untracked.rs"), "untracked\n")
        .expect("write untracked file");
    let dirty = GitRepositorySource::inspect(directory.path(), None)
        .await
        .expect("inspect dirty repository");
    assert!(dirty.dirty);
    assert_eq!(dirty.commit, initial.commit);

    let files = GitRepositorySource::tracked_files(&dirty)
        .await
        .expect("list tracked files");
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["src/lib.rs", "web/app.ts"]
    );
    assert_eq!(files[0].language.as_str(), "rust");
    assert_eq!(files[1].language.as_str(), "typescript");
    assert!(files.iter().all(|file| !file.blob_oid.is_empty()));

    let source = GitRepositorySource;
    let resolution = source
        .resolve(&SourceRequest {
            locator: directory.path().to_string_lossy().into_owned(),
            version: None,
        })
        .await
        .expect("resolve dirty source");
    let artifacts = source
        .artifacts(&resolution.input.sources[0])
        .await
        .expect("list artifacts");
    let rust = artifacts
        .into_iter()
        .find(|artifact| artifact.path == "src/lib.rs")
        .expect("Rust artifact");
    let content = source
        .read_many(&resolution.input.sources[0], &[rust])
        .await
        .expect("read committed blob");
    assert_eq!(content[0].bytes, b"pub fn run() {}\n");
}

#[tokio::test]
async fn source_and_analyzer_produce_a_partial_code_snapshot() {
    let directory = fixture();
    let source = GitRepositorySource;
    let resolution = source
        .resolve(&SourceRequest {
            locator: directory.path().to_string_lossy().into_owned(),
            version: None,
        })
        .await
        .expect("resolve source");
    let analyzer: Arc<dyn ModelAnalyzer> = Arc::new(GitRepositoryAnalyzer);
    let batch = analyzer
        .analyze(&resolution.input)
        .await
        .expect("analyze repository");
    assert_eq!(batch.snapshot, resolution.input.snapshot);
    assert_eq!(batch.coverage, AnalysisCoverage::Partial);
    assert_eq!(batch.entities.len(), 2);
    assert!(
        batch
            .entities
            .iter()
            .all(|entity| entity.entity.kind == "file")
    );
    assert!(batch.entities.iter().all(|entity| {
        entity
            .measurements
            .iter()
            .any(|measurement| measurement.name == "git.total_commits")
    }));
    assert!(batch.relations.is_empty());
}

#[tokio::test]
async fn history_follows_renames_and_stops_at_the_requested_snapshot() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let root = directory.path();
    run_git(root, &["init", "-q"]);
    run_git(root, &["checkout", "-q", "-b", "main"]);
    fs::create_dir_all(root.join("src")).expect("create source directory");

    fs::write(root.join("src/old.rs"), "pub fn value() -> u8 { 1 }\n").expect("first");
    run_git(root, &["add", "."]);
    commit_at(
        root,
        "first",
        "2024-01-01T00:00:00Z",
        "Ada",
        "ada@example.test",
    );

    fs::write(root.join("src/old.rs"), "pub fn value() -> u8 { 2 }\n").expect("second");
    run_git(root, &["add", "."]);
    commit_at(
        root,
        "second",
        "2024-01-11T00:00:00Z",
        "Grace",
        "grace@example.test",
    );

    fs::write(root.join("src/old.rs"), "pub fn value() -> u8 { 3 }\n").expect("third");
    run_git(root, &["add", "."]);
    commit_at(
        root,
        "third",
        "2024-01-21T00:00:00Z",
        "Ada",
        "ada@example.test",
    );

    run_git(root, &["mv", "src/old.rs", "src/current.rs"]);
    commit_at(
        root,
        "rename",
        "2024-02-10T00:00:00Z",
        "Grace",
        "grace@example.test",
    );
    let renamed_snapshot = head(root);

    fs::write(root.join("src/current.rs"), "pub fn value() -> u8 { 4 }\n").expect("fifth");
    run_git(root, &["add", "."]);
    commit_at(
        root,
        "fifth",
        "2024-05-20T00:00:00Z",
        "Ada",
        "ada@example.test",
    );

    let old = GitRepositorySource::inspect(root, Some(&renamed_snapshot))
        .await
        .expect("old snapshot");
    let old_history = GitRepositorySource::file_histories(&old)
        .await
        .expect("old history");
    let old_file = old_history
        .get("src/current.rs")
        .expect("renamed file history");
    assert_eq!(old_file.total_commits, 4);
    assert_eq!(old_file.contributors, 2);
    assert_eq!(old_file.recent_commits, 4);
    assert_eq!(old_file.recent_frequency.as_str(), "medium");
    assert!((old_file.average_days_between_changes - (40.0 / 3.0)).abs() < 0.000_001);

    let current = GitRepositorySource::inspect(root, None)
        .await
        .expect("current snapshot");
    let current_history = GitRepositorySource::file_histories(&current)
        .await
        .expect("current history");
    let current_file = current_history
        .get("src/current.rs")
        .expect("current file history");
    assert_eq!(current_file.total_commits, 5);
    assert_eq!(current_file.contributors, 2);
    assert_eq!(current_file.recent_commits, 1);
    assert_eq!(current_file.recent_frequency.as_str(), "low");
    assert!((current_file.average_days_between_changes - 35.0).abs() < 0.000_001);
    assert!(current_file.created_at.starts_with("2024-01-01T"));
    assert!(current_file.last_modified_at.starts_with("2024-05-20T"));
}
