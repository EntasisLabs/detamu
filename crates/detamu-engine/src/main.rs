use std::{process::ExitCode, sync::Arc};

use detamu_code_coverage::CodeCoverageDeriver;
use detamu_language::LanguagePack;
use detamu_language_lizard::LizardAnalyzer;
use detamu_language_rust::RustLanguagePack;
use detamu_language_rust_analyzer::RustAnalyzer;
use detamu_model::SourceRequest;
use detamu_model_code::{AvecCodeScorer, GraphMetricsDeriver};
use detamu_runtime::{RuntimeResolver, RuntimeSpec};
use detamu_sdk::Detamu;
use detamu_source_git::{GitRepositoryAnalyzer, GitRepositorySource};
use detamu_surreal::SurrealStore;

#[tokio::main]
async fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    match command.as_deref() {
        Some("version" | "--version" | "-V") => {
            println!("detamu {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("doctor") => {
            let inventory = runtime_inventory().await;
            let lizard = runtime_available(&inventory, "lizard");
            let rust_analyzer = runtime_available(&inventory, "rust-analyzer");
            let report = serde_json::json!({
                "name": "detamu",
                "version": env!("CARGO_PKG_VERSION"),
                "sdk": "available",
                "store": "in-memory",
                "surreal": "surrealkv",
                "world_models": ["code"],
                "language_packs": ["rust"],
                "coverage_formats": ["lcov", "cobertura"],
                "analysis_engines": {
                    "tree_sitter": true,
                    "lizard": lizard,
                    "lsp_host": true,
                    "rust_analyzer": rust_analyzer,
                },
                "runtime_contract": inventory,
            });
            println!("{report}");
            ExitCode::SUCCESS
        }
        Some("runtimes") => {
            let inventory = runtime_inventory().await;
            match serde_json::to_string(&inventory) {
                Ok(report) => {
                    println!("{report}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("failed to serialize runtime inventory: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("init") => {
            let Some(path) = arguments.next() else {
                eprintln!("usage: detamu init <PATH> [NAMESPACE] [DATABASE]");
                return ExitCode::from(2);
            };
            let namespace = arguments.next().unwrap_or_else(|| "detamu".to_owned());
            let database = arguments.next().unwrap_or_else(|| "detamu".to_owned());
            match SurrealStore::surrealkv(&path, &namespace, &database).await {
                Ok(_) => {
                    println!("initialized Detamu SurrealKV at {path}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("failed to initialize Detamu SurrealKV: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("index") => {
            let Some(repository) = arguments.next() else {
                eprintln!(
                    "usage: detamu index <REPOSITORY> <DATABASE_PATH> [NAMESPACE] [DATABASE]"
                );
                return ExitCode::from(2);
            };
            let Some(path) = arguments.next() else {
                eprintln!(
                    "usage: detamu index <REPOSITORY> <DATABASE_PATH> [NAMESPACE] [DATABASE]"
                );
                return ExitCode::from(2);
            };
            let options = match IndexOptions::parse(arguments) {
                Ok(options) => options,
                Err(message) => {
                    eprintln!("{message}");
                    return ExitCode::from(2);
                }
            };
            index_repository(&repository, &path, &options).await
        }
        Some(command @ ("snapshots" | "inspect" | "find" | "impact" | "diff" | "gaps")) => {
            query_commands::run(command, arguments).await
        }
        Some("help" | "--help" | "-h") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(unknown) => {
            eprintln!("unknown command: {unknown}\n");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!(
        "Detamu — a versioned world-model engine\n\n\
         Usage: detamu <COMMAND>\n\n\
         Commands:\n  \
           doctor    Report installed engine capabilities\n  \
           init      Initialize a native SurrealKV database\n  \
           index     Index a Git repository snapshot with optional coverage evidence\n  \
           snapshots List persisted immutable snapshots\n  \
           inspect   Inspect one entity and its relations\n  \
           find      Find code entities by path, name, kind, language, or line\n  \
           impact    Traverse reverse code dependencies\n  \
           diff      Compare two snapshots of the same world\n  \
           gaps      Explain missing AVEC evidence and scores\n  \
           runtimes  Report optional analyzer package requirements and resolution\n  \
           version   Print the engine version\n  \
           help      Print this help"
    );
}

async fn index_repository(repository: &str, path: &str, options: &IndexOptions) -> ExitCode {
    let coverage = if options.coverage.is_empty() {
        None
    } else {
        match CodeCoverageDeriver::from_paths(&options.coverage) {
            Ok(coverage) => Some(Arc::new(coverage)),
            Err(error) => {
                eprintln!("failed to load coverage evidence: {error}");
                return ExitCode::FAILURE;
            }
        }
    };
    let store = match SurrealStore::surrealkv(path, &options.namespace, &options.database).await {
        Ok(store) => Arc::new(store),
        Err(error) => {
            eprintln!("failed to open Detamu SurrealKV: {error}");
            return ExitCode::FAILURE;
        }
    };
    let rust = RustLanguagePack::new(Arc::new(GitRepositorySource));
    let resolver = RuntimeResolver::from_environment();
    let lizard_runtime = resolver.resolve(&RuntimeSpec::lizard()).await;
    let rust_analyzer_runtime = resolver.resolve(&RuntimeSpec::rust_analyzer()).await;
    let mut builder = Detamu::builder(store)
        .analyzer(Arc::new(GitRepositoryAnalyzer))
        .analyzers(rust.analyzers())
        .analyzer(Arc::new(LizardAnalyzer::with_executable(
            Arc::new(GitRepositorySource),
            lizard_runtime.executable,
        )))
        .analyzer(Arc::new(
            RustAnalyzer::new(Arc::new(GitRepositorySource))
                .with_executable(rust_analyzer_runtime.executable),
        ))
        .deriver(Arc::new(GraphMetricsDeriver));
    if let Some(coverage) = coverage {
        builder = builder.deriver(coverage);
    }
    let detamu = builder
        .scoring_model(Arc::new(AvecCodeScorer::default()))
        .build();
    let request = SourceRequest {
        locator: repository.to_owned(),
        version: options.revision.clone(),
    };
    match detamu.index_source(&GitRepositorySource, &request).await {
        Ok(report) => {
            let result = serde_json::json!({
                "world": report.snapshot.world.as_str(),
                "snapshot": report.snapshot.version.as_str(),
                "entities": report.entities,
                "relations": report.relations,
                "analyzers_run": report.analyzers_run,
                "analyzers_skipped": report.analyzers_skipped,
                "derivers_run": report.derivers_run,
                "coverage_reports": options.coverage.len(),
                "coverage": format!("{:?}", report.coverage).to_ascii_lowercase(),
            });
            println!("{result}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("failed to index repository: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn runtime_inventory() -> detamu_runtime::RuntimeInventory {
    RuntimeResolver::from_environment()
        .inventory(&[RuntimeSpec::lizard(), RuntimeSpec::rust_analyzer()])
        .await
}

fn runtime_available(inventory: &detamu_runtime::RuntimeInventory, id: &str) -> bool {
    inventory
        .runtimes
        .iter()
        .any(|runtime| runtime.spec.id == id && runtime.available)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexOptions {
    namespace: String,
    database: String,
    coverage: Vec<String>,
    revision: Option<String>,
}

impl IndexOptions {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut coverage = Vec::new();
        let mut revision = None;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            if argument == "--coverage" {
                coverage.push(arguments.next().ok_or_else(index_usage)?);
            } else if argument == "--revision" {
                if revision.is_some() {
                    return Err("--revision may only be supplied once".to_owned());
                }
                revision = Some(arguments.next().ok_or_else(index_usage)?);
            } else if argument.starts_with('-') {
                return Err(format!(
                    "unknown index option: {argument}\n{}",
                    index_usage()
                ));
            } else {
                positional.push(argument);
            }
        }
        if positional.len() > 2 {
            return Err(index_usage());
        }
        Ok(Self {
            namespace: positional
                .first()
                .cloned()
                .unwrap_or_else(|| "detamu".to_owned()),
            database: positional
                .get(1)
                .cloned()
                .unwrap_or_else(|| "detamu".to_owned()),
            coverage,
            revision,
        })
    }
}

fn index_usage() -> String {
    "usage: detamu index <REPOSITORY> <DATABASE_PATH> [NAMESPACE] [DATABASE] \
     [--revision <COMMITISH>] [--coverage <LCOV_OR_COBERTURA_PATH>]..."
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeated_coverage_inputs_without_changing_positional_defaults() {
        let options = IndexOptions::parse([
            "--coverage".to_owned(),
            "lcov.info".to_owned(),
            "workspace".to_owned(),
            "analysis".to_owned(),
            "--coverage".to_owned(),
            "coverage.xml".to_owned(),
        ])
        .expect("parse index options");

        assert_eq!(options.namespace, "workspace");
        assert_eq!(options.database, "analysis");
        assert_eq!(options.coverage, ["lcov.info", "coverage.xml"]);
        assert_eq!(options.revision, None);
    }

    #[test]
    fn parses_an_explicit_immutable_revision() {
        let options = IndexOptions::parse(["--revision".to_owned(), "abc123".to_owned()])
            .expect("parse revision");

        assert_eq!(options.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn rejects_a_coverage_option_without_a_path() {
        let error = IndexOptions::parse(["--coverage".to_owned()]).expect_err("missing path");

        assert!(error.starts_with("usage: detamu index"));
    }
}
mod query_commands;
