# Detamu

[![crates.io](https://img.shields.io/crates/v/detamu.svg)](https://crates.io/crates/detamu)
[![docs.rs](https://img.shields.io/docsrs/detamu)](https://docs.rs/detamu)
[![license](https://img.shields.io/crates/l/detamu.svg)](https://github.com/EntasisLabs/detamu/blob/main/LICENSE-MIT)

**Index Git repositories into immutable, queryable code graphs.**

Detamu turns observations from bounded worlds into versioned entity graphs with
provenance, measurements, and scores. Code is the first world model: index a
commit, persist the snapshot, then look up symbols, trace reverse dependencies,
and score how risky each piece of code is to change.

Use it as an embeddable Rust SDK or a standalone CLI. The engine is a thin host
around the SDK — there is no second implementation.

Detamu is building toward ACC-compatible repository graphs and their scoring
behavior. It does not own agent runtimes or review workflows; host applications
remain authoritative for users and optional analyzer package lifecycle.

## What you can do today

- **Index a Git repository at an immutable commit** — tracked files, rename-aware
  history, Rust symbols via Tree-sitter, optional Lizard and rust-analyzer
  enrichment, and external LCOV/Cobertura coverage ingestion.
- **Query persisted snapshots** — filter entities, locate source lines, traverse
  reverse impact, diff snapshots, and list where scoring evidence is still
  missing instead of inventing zeros.
- **Score code with AVEC** — four versioned dimensions for each scoreable symbol
  (see below).
- **Embed or shell out** — compose analyzers through the SDK, or drive the same
  operations through JSON commands from `detamu-engine`.

## Scoring (AVEC Code)

AVEC Code is Detamu's built-in scoring model for the code world. When enough
measurements exist, each symbol gets four 0–1 scores:

| Dimension | Roughly answers | Drawn from |
|---|---|---|
| **Stability** | How settled is this code? | Churn, contributor count, test coverage |
| **Logic** | How dense is the implementation? | Cyclomatic complexity, size, parameters |
| **Friction** | How costly is a change here? | Graph centrality, inbound deps, history, complexity |
| **Autonomy** | How independent is it? | Outbound dependency load |

Scores are derived and versioned (`avec.code`); they are not raw analyzer output.
If required evidence is missing — for example no call graph yet, or no coverage
report — Detamu leaves the score out and `detamu gaps` explains why. Missing
evidence is never treated as “safe.”

## Try it in two minutes

Install the CLI ([`detamu-engine`](https://crates.io/crates/detamu-engine) on
crates.io):

```bash
cargo install detamu-engine
detamu doctor
detamu init ./data/detamu.surrealkv
detamu index . ./data/detamu.surrealkv
```

`index` prints the world and snapshot identifiers you need for queries:

```json
{
  "world": "code.repository:remote:github.com/your-org/your-repo",
  "snapshot": "83d4ba3f003799219ec5dcf28b1bb0a303bf2693",
  "entities": 720,
  "relations": 757,
  "coverage": "partial"
}
```

Query the graph (replace placeholders with your `index` output):

```bash
detamu snapshots ./data/detamu.surrealkv

detamu find ./data/detamu.surrealkv <WORLD> <SNAPSHOT> \
  --kind function --language rust --limit 10

detamu impact ./data/detamu.surrealkv <WORLD> <SNAPSHOT> <ENTITY_ID>

detamu gaps ./data/detamu.surrealkv <WORLD> <SNAPSHOT>
```

Index a specific commit without checking it out:

```bash
detamu index . ./data/detamu.surrealkv --revision abc123def
```

Optional analyzers (Lizard, rust-analyzer) are discovered from the environment
or a host-managed package directory — they are not bundled. See
[Analyzer runtimes](docs/RUNTIMES.md).

Full walkthrough: [Getting started](docs/GETTING_STARTED.md).

## Use it in Rust

```toml
[dependencies]
detamu = { version = "0.1", features = ["code", "runtime", "surreal"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Feature flags are additive: `query` and `runtime` (defaults), plus `code`,
`surreal`, or `full` for everything.

Runnable example from this repository:

```bash
cargo run -p detamu-index-and-query -- .
```

See [examples/](examples/) and [Querying Detamu](docs/QUERYING.md) for embedded
and JSON consumption contracts.

## How it works

```mermaid
flowchart LR
  Git[Git commit] --> Source[World source]
  Source --> Analyzers[Model analyzers]
  Analyzers --> Batch[Normalized observations]
  Batch --> Derive[Derivation and scoring]
  Derive --> Store[(Snapshot store)]
  Store --> Query[Query facades]
  Query --> SDK[Rust SDK]
  Query --> CLI[JSON CLI]
```

World sources resolve an immutable revision. Analyzers emit normalized
observations; optional tools degrade gracefully. Derivers attach graph metrics
and coverage; AVEC Code then scores symbols that have enough evidence. The store
commits each snapshot atomically. Generic and code-aware query facades read the
same persisted graph.

Details: [Architecture](ARCHITECTURE.md).

## Project status

**Works well**

- Git snapshot identity, tracked-file inventory, and bulk rename-aware history.
- In-process Rust Tree-sitter analysis without optional runtimes.
- SurrealKV persistence, snapshot listing, entity search, impact traversal,
  content-aware diffs, and scoring gap reports.
- Optional Lizard, rust-analyzer, and coverage report ingestion.

**Partial or optional**

- AVEC Code only scores when required measurements exist; incomplete analysis is
  reported by `gaps` rather than filled with zeros.
- Broad multi-language metrics depend on an installed Lizard binary.
- Call graphs and references depend on rust-analyzer when available.

**Next**

- C# semantic adapter over the generic LSP host.
- Richer import resolution and ACC graph golden comparisons.

## Documentation

| Doc | Contents |
|---|---|
| [Getting started](docs/GETTING_STARTED.md) | CLI install, SDK setup, coverage, persistence |
| [Querying Detamu](docs/QUERYING.md) | Rust facades and JSON command protocol |
| [Analyzer runtimes](docs/RUNTIMES.md) | Optional executable discovery and host packages |
| [Architecture](ARCHITECTURE.md) | Kernel boundaries, analyzers, storage, roadmap |
| [Publishing](docs/PUBLISHING.md) | crates.io release procedure |
| [Contributing](CONTRIBUTING.md) | Checks, constraints, and pull request expectations |
| [Examples](examples/) | Runnable workspace examples |

## Key crates

Most consumers should start with these:

| Crate | Role |
|---|---|
| [`detamu`](https://crates.io/crates/detamu) | Public facade (`code`, `query`, `runtime`, `surreal` features) |
| [`detamu-engine`](https://crates.io/crates/detamu-engine) | Standalone CLI and JSON protocol host |
| [`detamu-sdk`](https://crates.io/crates/detamu-sdk) | Model-agnostic orchestration for embedders |
| [`detamu-source-git`](https://crates.io/crates/detamu-source-git) | Git repository source adapter |

The workspace also contains kernel types, the code ontology, language adapters,
storage backends, and query facades. See [Architecture](ARCHITECTURE.md) for the
full crate map and dependency direction.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See [Contributing](CONTRIBUTING.md) for formatting, architecture constraints,
and release checks.

## License

Detamu is dual-licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE).
