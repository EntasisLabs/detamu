# Getting started with Detamu

Detamu turns observations from bounded worlds into immutable, queryable entity
graphs. Code is the first world model: index a Git repository, persist the
snapshot, and query symbols, dependencies, and versioned change-risk scores
(AVEC Code: stability, logic, friction, autonomy).

This guide covers the standalone CLI and an embedded Rust workflow. For deeper
contracts see [Querying Detamu](QUERYING.md), [Analyzer runtimes](RUNTIMES.md),
and [Architecture](../ARCHITECTURE.md).

## Install the CLI

The standalone engine is published as `detamu-engine` and installs the `detamu`
binary. `cargo install` builds it from crates.io. After a `v*` tag is pushed,
GitHub Release archives cover Linux x86_64, macOS arm64, macOS x86_64, and
Windows x86_64. `scripts/install.sh` downloads one and checks it against
`sha256sums.txt`.

```bash
cargo install detamu-engine

# After a release tag exists. Pin one with DETAMU_VERSION=v0.2.0.
curl -fsSL https://raw.githubusercontent.com/EntasisLabs/detamu/main/scripts/install.sh | bash

detamu --version
```

Optional analyzers (Lizard, rust-analyzer, and any language server you register)
are discovered from your environment or a host-managed package directory. See
[Analyzer runtimes](RUNTIMES.md).

## Five-minute CLI workflow

From any Git repository:

```bash
# Check what analysis engines are available on this machine
detamu doctor

# Create a persistent SurrealKV database
detamu init ./data/detamu.surrealkv

# Index HEAD into the database (Tree-sitter Rust analysis always runs)
detamu index . ./data/detamu.surrealkv

# List persisted snapshots
detamu snapshots ./data/detamu.surrealkv

# Find Rust functions
detamu find ./data/detamu.surrealkv <WORLD> <SNAPSHOT> \
  --kind function --language rust --limit 10

# Explain where scoring evidence is still missing
detamu gaps ./data/detamu.surrealkv <WORLD> <SNAPSHOT>

# ACC-style lookups on the same snapshot
detamu dependencies ./data/detamu.surrealkv <WORLD> <SNAPSHOT> <ENTITY_ID>
detamu friction ./data/detamu.surrealkv <WORLD> <SNAPSHOT>
detamu unstable ./data/detamu.surrealkv <WORLD> <SNAPSHOT>
detamu stats ./data/detamu.surrealkv <WORLD> <SNAPSHOT>
```

Replace `<WORLD>` and `<SNAPSHOT>` with the values printed by the `index`
command. Snapshot identity is the Git commit OID.

Register a language server before the next index, then serve the same queries
over JSON-RPC (methods accept a `detamu.` or `acc.` prefix):

```bash
detamu lsp register ./data/detamu.surrealkv \
  --id python --language python --command pyright-langserver \
  --arg --stdio --ext py

detamu serve ./data/detamu.surrealkv
```

### Historical snapshots

Index a specific commit without changing branch checkout:

```bash
detamu index . ./data/detamu.surrealkv --revision abc123def
```

Both commits remain queryable side by side.

### Optional coverage evidence

Detamu consumes external LCOV or Cobertura reports; it does not run tests:

```bash
detamu index . ./data/detamu.surrealkv \
  --coverage ./coverage/lcov.info \
  --coverage ./coverage/cobertura.xml
```

## Add the SDK to a Rust project

Most embedders depend on the public facade:

```toml
[dependencies]
detamu = { version = "0.2", features = ["code", "runtime", "surreal"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Feature flags are additive:

| Feature | Enables |
|---|---|
| `query` (default) | Snapshot lookup, filtering, traversal, diffs |
| `runtime` (default) | Optional analyzer executable discovery |
| `code` | Code ontology and code-aware queries |
| `rpc` | JSON-RPC handler for code queries and language-server registration |
| `surreal` | In-memory SurrealDB and persistent SurrealKV |
| `full` | All integrations |

Specialized hosts can depend on individual crates (`detamu-sdk`,
`detamu-source-git`, language adapters, and so on). See
[Publishing](PUBLISHING.md).

## Embedded indexing example

The [`examples/index-and-query`](../examples/index-and-query/) crate shows the
same pipeline the CLI uses, but with an in-memory store and no external database
file:

```bash
cargo run -p detamu-index-and-query -- .
```

The example:

1. resolves the repository at `HEAD` through `detamu-source-git`;
2. runs the Git inventory and Tree-sitter Rust analyzers;
3. scores entities with AVEC Code (stability, logic, friction, autonomy);
4. queries Rust functions from the resulting snapshot.

Optional analyzers (Lizard, rust-analyzer) are omitted so the example runs
everywhere Tree-sitter and Git are available. Add them the same way
`detamu-engine` does when you need broader language coverage or semantic call
graphs.

## Persistent storage from Rust

Use SurrealKV when snapshots should survive process restarts:

```rust,no_run
use std::sync::Arc;
use detamu_surreal::SurrealStore;

# async fn open() -> Result<(), detamu_surreal::SurrealError> {
let store = Arc::new(
    SurrealStore::surrealkv("./data/detamu.surrealkv", "detamu", "detamu").await?,
);
# Ok(())
# }
```

Pass the store to `Detamu::builder` and the query facades as `Arc<dyn DetamuStore>`.

## Query surfaces

Detamu exposes the same persisted world through:

- **Rust facades** — `detamu-query` for generic graph operations,
  `detamu-query-code` for source locations, reverse impact, and scoring gap reports;
- **JSON commands** — `detamu snapshots`, `find`, `impact`, `dependencies`,
  `patterns`, `friction`, `unstable`, `stats`, `diff`, and `gaps` for non-Rust
  clients;
- **JSON-RPC** — `detamu serve` on port 9339, or `with_listener` from the SDK.

See [Querying Detamu](QUERYING.md) for protocol details and Rust snippets.

## What to expect today

Detamu 0.2 indexes a commit, serves ACC-style queries, and can hand analysis to
any language server you register:

- Rust Tree-sitter analysis runs in-process. Source reads stay near 4 MiB per
  group, and Git history is streamed.
- Lizard, rust-analyzer, and registered language servers deepen metrics and call
  graphs when those programs are installed.
- AVEC Code scores require complete evidence. `detamu gaps` reports what is
  still missing.
- The next milestones are richer import resolution and ACC golden comparisons.

## Development

Clone the repository and use the pinned stable toolchain:

```bash
git clone https://github.com/EntasisLabs/detamu.git
cd detamu
cargo test --workspace --locked
cargo run -p detamu-engine -- doctor
```

Before publishing crate updates, run `./scripts/publish-crates.sh check`. See
[Publishing](PUBLISHING.md).

## License

Detamu is dual-licensed under MIT OR Apache-2.0. See [LICENSE-MIT](../LICENSE-MIT)
and [LICENSE-APACHE](../LICENSE-APACHE).
