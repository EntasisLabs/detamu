# Detamu

[![crates.io](https://img.shields.io/crates/v/detamu.svg)](https://crates.io/crates/detamu)
[![docs.rs](https://img.shields.io/docsrs/detamu)](https://docs.rs/detamu)
[![license](https://img.shields.io/crates/l/detamu.svg)](https://github.com/EntasisLabs/detamu/blob/main/LICENSE-MIT)

**A versioned world-model engine, beginning with code.**

Detamu turns observations from bounded worlds into immutable, queryable entity
graphs with provenance, measurements, and versioned scores. Code is the first
world model: Detamu will reproduce ACC's repository graph and AVEC behavior before
expanding into pull requests, issues, tickets, notes, and projects.

Detamu runs as either an embeddable Rust SDK or a standalone engine. The engine
is a thin host around the SDK; it does not contain a second implementation.

## Quick start

Install the CLI:

```bash
cargo install detamu-engine
detamu init ./data/detamu.surrealkv
detamu index . ./data/detamu.surrealkv
detamu snapshots ./data/detamu.surrealkv
```

Embed the SDK:

```toml
[dependencies]
detamu = { version = "0.1", features = ["code", "runtime", "surreal"] }
```

Run the in-memory example against this repository:

```bash
cargo run -p detamu-index-and-query -- .
```

See [Getting started](docs/GETTING_STARTED.md) for the full CLI and SDK walkthrough,
[examples/](examples/) for runnable code, and [Contributing](CONTRIBUTING.md) if
you want to hack on the workspace.

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
- deterministic snapshot enumeration, entity filtering, bounded graph traversal,
  and content-aware snapshot diffs;
- a separate code query facade for source-location lookup, reverse-dependency
  impact, and explicit AVEC analysis-gap reporting;
- portable optional-runtime discovery with bounded probes, tested-version
  metadata, managed package roots, and machine-readable status;
- an embeddable orchestration SDK and thin standalone engine.

The next milestone is a C# semantic adapter over the generic LSP host, followed
by richer import resolution and ACC graph golden comparisons.

## Workspace

| Crate | Responsibility |
|---|---|
| `detamu` | Public facade with additive query, code, runtime, and Surreal features |
| `detamu-core` | World-model-agnostic kernel types |
| `detamu-model` | Analyzer, scoring-model, and world-model-pack contracts |
| `detamu-model-code` | Code ontology, Git identity, metrics, and AVEC Code |
| `detamu-code-coverage` | LCOV and Cobertura evidence mapped onto code entities |
| `detamu-language` | Language extensions within the code model |
| `detamu-language-tree-sitter` | Shared immutable-artifact parsing lifecycle |
| `detamu-language-rust` | Rust symbols, hierarchy, and syntax complexity |
| `detamu-language-rust-analyzer` | Optional Rust references and call graph |
| `detamu-language-lizard` | Optional broad-coverage ACC metrics compatibility |
| `detamu-language-lsp` | Generic LSP stdio lifecycle and adapter boundary |
| `detamu-source-git` | Git discovery, snapshot resolution, and tracked-file inventory |
| `detamu-store` | Generic storage port and in-memory reference |
| `detamu-surreal` | Native in-memory SurrealDB and persistent SurrealKV backend |
| `detamu-query` | Generic snapshot lookup, filtering, traversal, and diffing |
| `detamu-query-code` | Code location, impact, and AVEC analysis-gap queries |
| `detamu-runtime` | Optional analyzer package discovery and status contract |
| `detamu-sdk` | Model-agnostic orchestration facade |
| `detamu-engine` | Standalone process and protocol host |

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p detamu-engine -- doctor
cargo run -p detamu-engine -- runtimes
cargo run -p detamu-engine -- init ./data/detamu.surrealkv
cargo run -p detamu-engine -- index . ./data/detamu.surrealkv
cargo run -p detamu-engine -- index . ./data/detamu.surrealkv \
  --coverage ./coverage/lcov.info --coverage ./coverage/cobertura.xml
cargo run -p detamu-engine -- snapshots ./data/detamu.surrealkv
./scripts/publish-crates.sh check
```

Set `DETAMU_RUNTIME_DIR` to a host-managed package root, or use
`DETAMU_LIZARD` / `DETAMU_RUST_ANALYZER` for explicit executable overrides.
`detamu runtimes` reports the resolution source, installed version, tested
versions, and failure details as versioned JSON.
Coverage reports are optional external evidence; Detamu consumes them but does
not run test suites. SDK consumers can construct `CodeCoverageDeriver` from
report bytes or filesystem paths.

See [Querying Detamu](docs/QUERYING.md) for the Rust and JSON consumption
contracts, [Analyzer runtimes](docs/RUNTIMES.md) for the Medousa package handoff,
[Publishing](docs/PUBLISHING.md) for the crates.io release procedure, and
[ARCHITECTURE.md](ARCHITECTURE.md) for the boundaries that should remain stable
as models and analyzers are added.

## License

Detamu is dual-licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
