# Contributing to Detamu

Thank you for helping improve Detamu. This project is a versioned world-model
engine with strict boundaries between the kernel, model packs, analyzers, storage,
and consumers.

## Before you open a pull request

1. Read [ARCHITECTURE.md](ARCHITECTURE.md) and [AGENTS.md](AGENTS.md).
2. Run the workspace checks from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --locked
```

GitHub Actions runs the same commands on pull requests and on pushes to `main`.
The live rust-analyzer test prints a skip line and returns unless
`DETAMU_RUST_ANALYZER` points at an executable. Lizard tests use checked-in CSV
fixtures.

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
