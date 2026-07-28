use std::{process::ExitCode, sync::Arc};

use detamu_model::SourceRequest;
use detamu_model_code::AvecCodeScorer;
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
            let report = serde_json::json!({
                "name": "detamu",
                "version": env!("CARGO_PKG_VERSION"),
                "sdk": "available",
                "store": "in-memory",
                "surreal": "surrealkv",
                "world_models": ["code"],
                "language_packs": [],
            });
            println!("{report}");
            ExitCode::SUCCESS
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
            let namespace = arguments.next().unwrap_or_else(|| "detamu".to_owned());
            let database = arguments.next().unwrap_or_else(|| "detamu".to_owned());
            index_repository(&repository, &path, &namespace, &database).await
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
           index     Index a Git repository snapshot\n  \
           version   Print the engine version\n  \
           help      Print this help"
    );
}

async fn index_repository(
    repository: &str,
    path: &str,
    namespace: &str,
    database: &str,
) -> ExitCode {
    let store = match SurrealStore::surrealkv(path, namespace, database).await {
        Ok(store) => Arc::new(store),
        Err(error) => {
            eprintln!("failed to open Detamu SurrealKV: {error}");
            return ExitCode::FAILURE;
        }
    };
    let detamu = Detamu::builder(store)
        .analyzer(Arc::new(GitRepositoryAnalyzer))
        .scoring_model(Arc::new(AvecCodeScorer::default()))
        .build();
    let request = SourceRequest {
        locator: repository.to_owned(),
        version: None,
    };
    match detamu.index_source(&GitRepositorySource, &request).await {
        Ok(report) => {
            let result = serde_json::json!({
                "world": report.snapshot.world.as_str(),
                "snapshot": report.snapshot.version.as_str(),
                "entities": report.entities,
                "relations": report.relations,
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
