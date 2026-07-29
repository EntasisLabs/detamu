# Analyzer runtimes

Detamu's in-process kernel, Tree-sitter analysis, storage, and query surfaces do
not require external analyzer runtimes. Lizard and language servers are optional
capability packages. Detamu discovers and reports those executables; Medousa or
another host owns download, checksum verification, upgrades, rollback, and
removal.

## Host package contract

Set `DETAMU_RUNTIME_DIR` to the host's package data directory. Detamu accepts both
of these layouts so a host may pass either its data root or its binary directory:

```text
<runtime-directory>/bin/lizard
<runtime-directory>/bin/rust-analyzer

<runtime-directory>/lizard
<runtime-directory>/rust-analyzer
```

Use `.exe` names on Windows. The package IDs and executable names are intentionally
stable and independent of a particular package registry.

Resolution precedence is:

1. The tool-specific override, such as `DETAMU_LIZARD` or
   `DETAMU_RUST_ANALYZER`.
2. An executable beside the Detamu engine, for bundled sidecars.
3. `DETAMU_RUNTIME_DIR/bin`, then `DETAMU_RUNTIME_DIR`.
4. The process `PATH`.

An explicit tool override is authoritative. If it is invalid, Detamu reports it
as unavailable rather than silently selecting a different installation.

## Machine-readable inventory

`detamu runtimes` emits the complete versioned contract as one JSON value:

```json
{
  "schema_version": 1,
  "runtime_directory_environment": "DETAMU_RUNTIME_DIR",
  "runtimes": [
    {
      "spec": {
        "id": "lizard",
        "executable": "lizard",
        "environment_override": "DETAMU_LIZARD",
        "version_arguments": ["--version"],
        "tested_versions": ["1.23.0"],
        "optional": true,
        "capabilities": ["symbols", "metrics"]
      },
      "available": true,
      "executable": "/host/data/bin/lizard",
      "source": "managed_directory",
      "version": "1.23.0",
      "detail": null
    }
  ]
}
```

The inventory always includes missing packages, allowing Medousa Packages to
compare desired and installed capabilities without parsing logs. Version probes
are bounded to five seconds. An empty `tested_versions` list means Detamu has not
yet declared a known-good executable release; availability alone must not be
presented as compatibility certification.

`detamu doctor` retains its simple analysis-engine booleans and includes the same
inventory under `runtime_contract`.

## Embedded hosts

Rust hosts can use `detamu-runtime` without starting the standalone engine:

```rust,no_run
use detamu_runtime::{RuntimeResolver, RuntimeSpec};

# async fn inspect() {
let resolver = RuntimeResolver::from_environment();
let inventory = resolver
    .inventory(&[RuntimeSpec::lizard(), RuntimeSpec::rust_analyzer()])
    .await;
for runtime in inventory.runtimes {
    println!("{}: {}", runtime.spec.id, runtime.available);
}
# }
```

After resolution, pass `RuntimeStatus::executable` to the corresponding analyzer's
`with_executable` constructor. This keeps package authority outside analyzer and
world-model code.

## Adding another language server

A future language adapter declares a `RuntimeSpec` with its stable package ID,
executable, override variable, version probe, tested versions, and capabilities.
The adapter itself remains responsible for LSP initialization and normalization
into Detamu observations. No new runtime installation mechanism or kernel concept
is required.
