# Detamu

**A versioned world-model engine, beginning with code.**

Detamu turns observations from bounded worlds into immutable, queryable entity
graphs with provenance, measurements, and versioned scores. Code is the first
world model: Detamu will reproduce ACC's repository graph and AVEC behavior before
expanding into pull requests, issues, tickets, notes, and projects.

Detamu runs as either an embeddable Rust SDK or a standalone engine. The engine
is a thin host around the SDK; it does not contain a second implementation.

## Status

The current workspace establishes:

- a world-model-agnostic kernel for snapshots, entities, relations, observations,
  measurements, provenance, and scores;
- model analyzer, scoring, and pack extension contracts;
- a strongly typed code model containing Git identity, symbols, dependencies,
  ACC metrics, and AVEC Code;
- deterministic Git repository discovery, immutable commit snapshots, and
  tracked-file language inventory;
- rename-aware bulk Git history with contributor, churn, timing, and recent
  frequency evidence anchored to each snapshot;
- immutable artifact access and a reusable Tree-sitter host, with a Rust pack
  producing types, functions, methods, imports, syntax metrics, and containment;
- an optional Lizard compatibility adapter for broad baseline language coverage;
- a generic, process-isolated LSP lifecycle and JSON-RPC transport for future
  semantic language adapters;
- an optional rust-analyzer adapter that materializes the complete immutable Git
  tree and normalizes workspace references and calls into code relations;
- a reconciled-batch derivation stage producing graph degree measurements before
  AVEC scoring;
- typed analyzer capabilities, optional-tool degradation, and per-measurement
  provenance/confidence for deterministic multi-analyzer reconciliation;
- generic storage with an in-memory behavioral reference;
- native SurrealDB and persistent SurrealKV storage with transactional bulk
  snapshot replacement;
- an embeddable orchestration SDK and thin standalone engine.

The next milestone is live rust-analyzer compatibility fixtures, richer import
resolution, and ACC graph golden comparisons.

## Workspace

| Crate | Responsibility |
|---|---|
| `detamu-core` | World-model-agnostic kernel types |
| `detamu-model` | Analyzer, scoring-model, and world-model-pack contracts |
| `detamu-model-code` | Code ontology, Git identity, metrics, and AVEC Code |
| `detamu-language` | Language extensions within the code model |
| `detamu-language-tree-sitter` | Shared immutable-artifact parsing lifecycle |
| `detamu-language-rust` | Rust symbols, hierarchy, and syntax complexity |
| `detamu-language-rust-analyzer` | Optional Rust references and call graph |
| `detamu-language-lizard` | Optional broad-coverage ACC metrics compatibility |
| `detamu-language-lsp` | Generic LSP stdio lifecycle and adapter boundary |
| `detamu-source-git` | Git discovery, snapshot resolution, and tracked-file inventory |
| `detamu-store` | Generic storage port and in-memory reference |
| `detamu-surreal` | Native in-memory SurrealDB and persistent SurrealKV backend |
| `detamu-sdk` | Model-agnostic orchestration facade |
| `detamu-engine` | Standalone process and protocol host |

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p detamu-engine -- doctor
cargo run -p detamu-engine -- init ./data/detamu.surrealkv
cargo run -p detamu-engine -- index . ./data/detamu.surrealkv
```

Set `DETAMU_RUST_ANALYZER` to an explicit rust-analyzer binary when it is not
available on `PATH`. `detamu doctor` reports whether the configured binary works.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the boundaries that should remain
stable as models and analyzers are added.
