# Detamu

**A living reference model for codebases.**

Detamu indexes code into a revision-aware semantic graph. It combines source
structure, dependency relationships, Git history, complexity, and coverage into
queryable observations and AVEC dimensional scores.

Detamu is designed to run in two forms:

- as an embeddable Rust SDK;
- as a standalone engine for editors, CI systems, agents, and other tools.

The engine is a host around the SDK. It does not contain a second implementation.

## Status

Detamu is in its initial Rust bootstrap. The workspace currently establishes:

- revision-aware domain types and AVEC scoring;
- pluggable analyzer and language-pack contracts;
- a storage interface with an in-memory reference implementation;
- an embeddable SDK facade;
- a thin standalone engine binary.

Native SurrealDB persistence and the first language pack are the next milestones.

## Workspace

| Crate | Responsibility |
|---|---|
| `detamu-core` | Domain types, observations, provenance, and AVEC |
| `detamu-language` | Analyzer and language-pack extension contracts |
| `detamu-store` | Storage contract and in-memory implementation |
| `detamu-sdk` | Embeddable orchestration facade |
| `detamu-engine` | Standalone process and protocol host |

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p detamu-engine -- doctor
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the boundaries that should remain
stable as the implementation grows.

