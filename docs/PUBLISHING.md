# Publishing Detamu crates

Detamu uses one synchronized workspace version. Every internal dependency carries
both a local `path` and a crates.io `version`, so local development uses the
workspace while published manifests resolve entirely through the registry.

## Public entry points

Most consumers should start with the `detamu` facade:

```toml
[dependencies]
detamu = "0.1"
```

Its default features expose generic queries and runtime discovery in addition to
the kernel, model contracts, SDK, and store contract. Features are additive:

```toml
detamu = { version = "0.1", features = ["code", "surreal"] }
```

- `query`: generic snapshot filtering, traversal, and diffs;
- `runtime`: optional analyzer runtime discovery;
- `code`: the code ontology and code-aware query facade; implies `query`;
- `surreal`: in-memory SurrealDB and persistent SurrealKV storage;
- `full`: all facade integrations.

Specialized embedders may depend directly on crates such as `detamu-sdk`,
`detamu-source-git`, or a language adapter. The standalone CLI installs with:

```bash
cargo install detamu-engine
```

## Preflight

The namespace was unclaimed when the initial release was prepared. Recheck before
the first publish:

```bash
cargo search detamu --limit 100
```

Run the guarded release check from a clean toolchain environment:

```bash
./scripts/publish-crates.sh check
```

This runs formatting, warning-denied Clippy with all features, all-feature tests,
and archive generation for every workspace crate. Archive generation uses
`--no-verify` because an initial multi-crate release cannot registry-resolve its
dependencies until the earlier crates have been published; workspace compilation
and tests provide the build verification before release.

Review packaged contents when needed with:

```bash
cargo package -p detamu --list
cargo package -p detamu-engine --list
```

## Initial release

1. Commit all release changes and ensure the worktree is clean.
2. Authenticate using `cargo login` or the standard Cargo registry token
   environment.
3. Run the explicit publishing mode:

```bash
DETAMU_PUBLISH=1 ./scripts/publish-crates.sh publish
```

The script publishes in dependency order and waits for each version to appear in
the crates.io index before publishing dependents. It refuses a dirty worktree and
requires the `DETAMU_PUBLISH=1` guard. Crates.io releases are immutable; do not
rerun the complete script after a partial publication without first identifying
the last successful crate.

The order is:

```text
detamu-core
detamu-runtime
detamu-model
detamu-store
detamu-model-code
detamu-language
detamu-language-lsp
detamu-language-tree-sitter
detamu-query
detamu-sdk
detamu-source-git
detamu-surreal
detamu-code-coverage
detamu-language-lizard
detamu-language-rust
detamu-language-rust-analyzer
detamu-query-code
detamu
detamu-engine
```

After the release, verify the facade from outside the workspace and tag the exact
published commit:

```bash
cargo info detamu@0.1.0
git tag v0.1.0
git push origin v0.1.0
```

## Medousa dependency

For embedded querying, runtime discovery, and Surreal storage, Medousa can use:

```toml
[dependencies]
detamu = { version = "0.1", features = ["code", "runtime", "surreal"] }
```

This does not install Lizard or language-server executables. Medousa Packages
continues to own those binaries and points Detamu at `{dataDir}` using
`DETAMU_RUNTIME_DIR`.

If Medousa needs to run indexing in-process, add the analyzer crates it actually
hosts rather than enabling a monolithic agent bundle:

```toml
detamu-code-coverage = "0.1"
detamu-language-lizard = "0.1"
detamu-language-rust = "0.1"
detamu-language-rust-analyzer = "0.1"
detamu-source-git = "0.1"
```

This preserves Detamu's hexagonal boundary: the facade supplies contracts and
composable implementations, while Medousa chooses the source, analyzers, package
lifecycle, and user workflow.
