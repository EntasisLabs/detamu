# Examples

Runnable examples for embedding Detamu. Each example is a workspace member so
`cargo run -p …` works from the repository root.

| Example | Command | What it shows |
|---|---|---|
| [index-and-query](index-and-query/) | `cargo run -p detamu-index-and-query -- .` | Index a Git repository in memory and query Rust functions |

These examples intentionally use the minimal analyzer set (Git inventory +
Tree-sitter Rust + AVEC scoring). Compare with `crates/detamu-engine` for the
full CLI stack including optional Lizard, rust-analyzer, coverage ingestion, and
SurrealKV persistence.

See [Getting started](../docs/GETTING_STARTED.md) for install instructions and
the CLI workflow.
