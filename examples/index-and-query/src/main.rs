//! Index a Git repository into an in-memory Detamu store and query Rust symbols.
//!
//! Run from the repository root:
//!
//! ```text
//! cargo run -p detamu-index-and-query -- .
//! cargo run -p detamu-index-and-query -- /path/to/other/repo
//! ```

use std::sync::Arc;

use detamu_language::LanguagePack;
use detamu_language_rust::RustLanguagePack;
use detamu_model::SourceRequest;
use detamu_model_code::AvecCodeScorer;
use detamu_query_code::{CodeEntityFilter, CodeEntitySummary, CodeQuery};
use detamu_sdk::Detamu;
use detamu_source_git::{GitRepositoryAnalyzer, GitRepositorySource};
use detamu_store::InMemoryStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());

    let store = Arc::new(InMemoryStore::default());
    let rust = RustLanguagePack::new(Arc::new(GitRepositorySource));
    let detamu = Detamu::builder(store.clone())
        .analyzer(Arc::new(GitRepositoryAnalyzer))
        .analyzers(rust.analyzers())
        .scoring_model(Arc::new(AvecCodeScorer::default()))
        .build();

    let report = detamu
        .index_source(
            &GitRepositorySource,
            &SourceRequest {
                locator: repository.clone(),
                version: None,
            },
        )
        .await?;

    println!("Indexed repository: {repository}");
    println!("  world:      {}", report.snapshot.world.as_str());
    println!("  snapshot:   {}", report.snapshot.version.as_str());
    println!("  entities:   {}", report.entities);
    println!("  relations:  {}", report.relations);
    println!("  coverage:   {:?}", report.coverage);

    let query = CodeQuery::new(store);
    let functions = query
        .find(
            &report.snapshot,
            &CodeEntityFilter {
                kind: Some("function".to_owned()),
                language: Some("rust".to_owned()),
                limit: Some(10),
                ..CodeEntityFilter::default()
            },
        )
        .await?;

    println!("\nFirst {} Rust functions:", functions.len());
    for observation in &functions {
        let summary = CodeEntitySummary::from(observation);
        let path = summary.path.as_deref().unwrap_or("<unknown>");
        let line = summary
            .line_start
            .map_or_else(|| "?".to_owned(), |value| value.to_string());
        println!("  {}:{}  {}", path, line, summary.label);
    }

    if let Some(first) = functions.first() {
        let first_summary = CodeEntitySummary::from(first);
        if let (Some(path), Some(line)) = (first_summary.path, first_summary.line_start)
            && let Some(at_line) = query.at_location(&report.snapshot, &path, line).await?
        {
            let summary = CodeEntitySummary::from(&at_line);
            println!(
                "\nNarrowest entity at {}:{} → {} ({})",
                path, line, summary.label, summary.kind
            );
        }
    }

    Ok(())
}
