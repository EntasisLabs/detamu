# Contributing to Detamu

Thank you for helping improve Detamu. This project is a versioned world-model
engine with strict boundaries between the kernel, model packs, analyzers, storage,
and consumers.

## Before you open a pull request

1. Read [ARCHITECTURE.md](ARCHITECTURE.md) and [AGENTS.md](AGENTS.md).
2. Run the workspace checks from the repository root:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

3. Keep changes scoped. Prefer extending model packs and analyzers through
   normalized observations rather than adding domain logic to `detamu-core` or
   `detamu-sdk`.

## Design constraints worth preserving

- Persist analysis against immutable revision identifiers, not branch names.
- Keep `detamu-core` free of database, process, and consumer dependencies.
- Represent partial or unavailable analysis explicitly; never treat missing
  evidence as zero.
- Prefer bulk persistence over per-entity round trips.
- Version scoring behavior and serialized contracts before changing semantics.

## Examples and documentation

User-facing docs live in `README.md`, `docs/`, and `examples/`. When you add a
feature that changes the CLI or SDK workflow, update the relevant doc and add or
extend an example if it helps newcomers reproduce the behavior.

## Publishing

Maintainers use `./scripts/publish-crates.sh` to release the workspace crates in
dependency order. See [docs/PUBLISHING.md](docs/PUBLISHING.md).

## License

By contributing, you agree that your contributions will be licensed under the
same terms as the project: MIT OR Apache-2.0.
