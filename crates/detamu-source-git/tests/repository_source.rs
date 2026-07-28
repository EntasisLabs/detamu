use std::{fs, path::Path, process::Command, sync::Arc};

use detamu_core::AnalysisCoverage;
use detamu_model::{ModelAnalyzer, SourceRequest, WorldSource};
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
    assert!(batch.relations.is_empty());
}
