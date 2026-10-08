# Publishing Detamu crates

Detamu uses one synchronized workspace version. Every internal dependency carries
both a local `path` and a crates.io `version`, so local development uses the
workspace while published manifests resolve entirely through the registry.

## Public entry points

Most consumers should start with the `detamu` facade:

```toml
[dependencies]
detamu = "0.2"
```

Its default features expose generic queries and runtime discovery in addition to
the kernel, model contracts, SDK, and store contract. Features are additive:

```toml
detamu = { version = "0.2", features = ["code", "surreal"] }
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

The workspace crates are published on [crates.io](https://crates.io/crates/detamu).
Before a new release, confirm the target version is still available:

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

Inspect the current release progress without uploading anything:

```bash
./scripts/publish-crates.sh status
```

## Shipping a release

Binary archives and crates.io are separate steps. The GitHub workflow publishes
the `detamu` binary only. crates.io stays on this script, run by hand, with no
registry token stored in the repository.

1. Land the version bump on `main`. `[workspace.package].version` and every
   internal `workspace.dependencies` pin must be the same value.
2. Tag that commit and push the tag. The tag must be `v` plus the workspace
   version (`v0.2.0` for workspace version `0.2.0`):

```bash
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)"
git tag "v${VERSION}"
git push origin "v${VERSION}"
```

3. [`.github/workflows/release.yml`](../.github/workflows/release.yml) runs on
   that tag. It builds release archives for Linux x86_64 (`ubuntu-24.04`),
   macOS arm64, macOS x86_64 (`macos-15-intel`), and Windows x86_64, refuses a
   tag that does not match the workspace version, and publishes a GitHub
   Release containing the archives, `sha256sums.txt`, and generated notes.
   `workflow_dispatch` can rebuild an existing `v*` tag. An empty dispatch input
   builds the current ref and does not publish a release.
4. Authenticate with `cargo login` or `CARGO_REGISTRY_TOKEN` on the machine
   that will upload, then publish the crates from a clean checkout of the
   tagged commit:

```bash
DETAMU_PUBLISH=1 ./scripts/publish-crates.sh publish
```

The script publishes in dependency order and waits for each version to appear in
the crates.io index before publishing dependents. It refuses a dirty worktree and
requires the `DETAMU_PUBLISH=1` guard. Before each upload it checks the exact
`crate@version` in crates.io and skips it when already published. If crates.io
rate-limits or interrupts a release, wait for the reported retry time and run
the same command again; it resumes at the first unpublished crate. A failed
upload is also rechecked in case the registry accepted it but Cargo lost the
response. `examples/index-and-query` is `publish = false` and is left out.

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
detamu-rpc
detamu
detamu-engine
```

After the crates are on the index, verify the facade and the CLI crate from
outside the workspace:

```bash
VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)"
cargo info "detamu@${VERSION}"
cargo info "detamu-engine@${VERSION}"
```

`scripts/install.sh` then installs the binary from the GitHub Release for that
tag.

## Medousa dependency

For embedded querying, runtime discovery, and Surreal storage, Medousa can use:

```toml
[dependencies]
detamu = { version = "0.2", features = ["code", "runtime", "surreal"] }
```

This does not install Lizard or language-server executables. Medousa Packages
continues to own those binaries and points Detamu at `{dataDir}` using
`DETAMU_RUNTIME_DIR`.

If Medousa needs to run indexing in-process, add the analyzer crates it actually
hosts rather than enabling a monolithic agent bundle:

```toml
detamu-code-coverage = "0.2"
detamu-language-lizard = "0.2"
detamu-language-rust = "0.2"
detamu-language-rust-analyzer = "0.2"
detamu-source-git = "0.2"
```

This preserves Detamu's hexagonal boundary: the facade supplies contracts and
composable implementations, while Medousa chooses the source, analyzers, package
lifecycle, and user workflow.
