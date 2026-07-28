# Detamu architecture

## Product boundary

Detamu observes and models code. It does not own an editor, an agent runtime, a
work lifecycle, or a review decision.

The core flow is:

```text
repository revision
  -> registered analyzers
  -> normalized observation batch
  -> AVEC calculation
  -> store
  -> graph and impact queries
```

Every persisted observation is bound to a repository and immutable Git revision.
Branches are useful input labels, but they are not durable analysis identities.

## Dependency direction

```text
detamu-core
  ^       ^
  |       |
language  store
   \       /
    detamu-sdk
        ^
        |
  detamu-engine
```

`detamu-core` must not depend on a database, process host, LSP implementation, or
consumer such as Medousa. The standalone engine must remain a thin host around
the SDK.

## Extension model

Analyzers emit a normalized `ObservationBatch`. A language pack is a discoverable
bundle of analyzers; LSP is one possible analyzer source rather than the language
abstraction itself. Packs may combine LSP, tree-sitter, build-system metadata,
coverage files, or external complexity engines.

Incomplete analysis is represented explicitly through coverage status and
diagnostics. Missing capabilities must never be silently interpreted as zero risk.

## Storage

The store contract is intentionally independent of SurrealDB. SurrealDB is the
first-class production backend; the in-memory implementation provides fast tests
and an executable specification for backend behavior.

Indexing backends should favor staged bulk writes and explicit index commits over
per-edge database events. AVEC is deterministic domain logic and is calculated in
Rust.

## Consumer integration

Consumers embed `detamu-sdk` or communicate with `detamu-engine`. A consumer may
attach a Detamu impact report to its own evidence or lifecycle, but Detamu does
not become authoritative for that lifecycle.

