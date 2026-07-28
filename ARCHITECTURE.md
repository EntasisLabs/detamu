# Detamu architecture

## Product boundary

Detamu is a versioned world-model engine. It observes bounded worlds, reconciles
evidence into immutable snapshots, derives relations and scores, and exposes the
result for queries and impact analysis.

Detamu does not own an agent runtime, user conversation, work lifecycle,
provider authority, or review decision. Medousa and other consumers remain
authoritative for their users and workflows.

Code is the first world model and ACC compatibility is its behavioral target. It
is not Detamu's universal ontology.

## Kernel vocabulary

The world-model-agnostic kernel contains:

- `WorldId`: a bounded modeled environment;
- `SnapshotId`: a world plus an immutable source/version identity;
- entities and typed relations;
- observations, measurements, provenance, coverage, and diagnostics;
- versioned, normalized scores;
- explicit commit semantics.

Domain meaning belongs to a world-model pack. The code pack owns repositories,
Git revisions, symbols, dependencies, code metrics, language support, and AVEC.
Future packs may model pull requests, tickets, notes, projects, or other worlds
without changing the kernel.

## Dependency direction

```text
                detamu-core
                  ^     ^
                  |     |
          detamu-model  detamu-store
             ^   ^          ^
             |   |          |
 model-code /    |      detamu-surreal
    ^    ^        \        /
language source-git detamu-sdk
                        ^
                        |
                  detamu-engine
```

`detamu-core` must not depend on a database, code vocabulary, provider SDK,
process host, or consumer. `detamu-sdk` orchestrates model analyzers and scoring
models without branching on domain. The standalone engine remains a thin host
around the SDK.

## Hexagonal ports

Inbound ports are the Rust SDK and standalone engine protocols. Outbound ports
are model analyzers, scoring models, storage, and eventually source synchronization
and event publication.

A `WorldModelPack` declares its model schema, analyzers, and scoring models.
Packs are compile-time registered until multiple real models demonstrate the
requirements for a stable dynamic plugin ABI.

`WorldSource` resolves a source-native locator and optional version into an
immutable Detamu snapshot plus analyzer input. A source may expose mutable labels
such as branches, but snapshot identity must use an immutable source version.

`ArtifactReader` exposes content-addressed artifacts at that immutable source
version. Language analyzers depend on this port rather than Git or the mutable
filesystem. The Git implementation uses one `cat-file --batch` process to read
all requested blobs and verifies every returned object identity.

Observers may enrich the same entity. Batch reconciliation merges non-conflicting
attributes, measurements, and scores by stable entity identity, deduplicates
identical relations, and rejects conflicting evidence from the same observer.
Measurements with the same semantic name from different observers coexist with
their provenance and confidence. Consumers select evidence deterministically by
confidence and observer identity; the kernel never silently overwrites evidence.

Analyzer descriptors declare typed capabilities and whether execution is required
or optional. An unavailable optional tool adds a diagnostic and makes coverage
partial without aborting the snapshot. Failed required analyzers and malformed
output still fail indexing.

## Git code source

`detamu-source-git` is the first source adapter. It:

- discovers the worktree root from any path inside a repository;
- derives repository identity from a credential-free normalized origin URL, or
  a hash of the canonical local root when no origin exists;
- resolves `HEAD` or an explicitly requested revision to a commit OID;
- treats branch and dirty state as metadata rather than durable identity;
- enumerates supported tracked files from the exact commit tree in deterministic
  path order;
- emits file entities with language, blob OID, Git mode, and byte size.
- extracts all per-file history in one chronological, rename-aware traversal;
- records created/modified time, commits, contributors, average days between
  changes, recent commits, and ACC-compatible low/medium/high frequency.

Inventory never reads the mutable filesystem. Later content analyzers must read
the corresponding Git blobs or otherwise prove that their input matches the
requested commit.

Recent activity uses ACC's 90-day and 0–2/3–9/10+ thresholds, but the window is
anchored to the snapshot commit's author time rather than wall-clock time. This
makes observations reproducible. Average change intervals use consecutive
observed commit times; negative author-date movement is clamped to zero.

## Rust language pack

`detamu-language-tree-sitter` owns artifact selection, immutable content reads,
parser execution, diagnostics, and provenance. A language pack supplies only its
grammar, extensions, capabilities, configuration version, and tree-to-observation
rules. `detamu-language-rust` is the first such specification and emits structs,
enums, unions, type aliases, traits, modules, functions, trait methods, and impl
methods. It produces:

- ACC-compatible SHA-256 location identities (`path:name:line`, first 16 bytes);
- one-based source ranges and qualified names;
- source signatures, LOC, parameter counts, and syntax complexity;
- inherited file-history evidence;
- `file -> contains -> symbol` relations.

Syntax complexity version `syntax-metrics-v1` starts functions at one and counts
branching and loop constructs, match arms, try expressions, and boolean decision
expressions. It is deterministic but not yet claimed to be byte-for-byte Lizard
parity. Missing graph and coverage evidence remains absent, so AVEC scoring skips
those entities until the required measurements exist.

## Layered language analysis

Detamu does not reimplement every language frontend. Code analysis is layered:

- `detamu-language-lizard` invokes an optional installed Lizard binary against a
  temporary materialization of immutable artifacts. Its CSV output supplies broad
  baseline functions, LOC, complexity, and parameter evidence with ACC-compatible
  identities. Missing Lizard never prevents other analyzers from completing.
- Tree-sitter language specifications provide fast, in-process structure and
  deterministic syntax observations.
- `detamu-language-lsp` provides a generic stdio process lifecycle, initialize /
  shutdown handshake, Content-Length JSON-RPC framing, request timeouts, and an
  adapter trait. Language-specific packs own LSP requests and normalization.

All layers emit the same normalized code model. Lizard has lower metric confidence
than the in-process Rust syntax specification, so both values remain inspectable
while scoring selects the higher-confidence evidence. LSP adapters can add deeper
references, calls, types, and diagnostics without changing the kernel.

## Observation and scoring flow

```text
world sources
  -> model analyzers
  -> normalized observation batches
  -> reconciliation / merge
  -> model scoring
  -> atomic snapshot commit
  -> graph, comparison, and impact queries
```

Incomplete analysis is represented explicitly through coverage and diagnostics.
Missing capabilities must never be interpreted as zero risk. Measurements are
source evidence; scores are derived and carry a scoring-model identity and
formula version.

## Commit semantics

Every batch declares its commit mode. The first implemented mode is
`ReplaceSnapshot`: atomically replace one complete immutable snapshot. Repeating
the same batch is idempotent and removes observations omitted by a later batch.

Delta synchronization and append-only facts will be added as explicit modes when
a mutable work-system source is implemented. They must not be inferred from an
incomplete batch.

## Storage

The store contract is independent of SurrealDB. SurrealDB is the first-class
production backend; the in-memory implementation is the behavioral reference.

Surreal persists generic snapshots, entity observations, and relation
observations. Common identity, model, kind, label, and endpoint fields are
indexed; versioned model payloads preserve typed domain attributes. World-model
specific tables or indexes may be added as projections, but cannot become kernel
requirements.

Bulk writes and the snapshot marker occur in one transaction. Readers observe
either the previous complete snapshot or the new one, never an intermediate
graph.

## Code-first roadmap

1. Keep the generic seams stable.
2. Port ACC behavior into `detamu-model-code`.
3. Add the first LSP adapter, Rust dependency extraction, and graph measurements.
4. Verify formulas, Lizard parity, and graph output with C# ACC golden fixtures.
5. Add languages through Tree-sitter specifications and LSP launch adapters.
6. Prove the architecture with a second pack for GitHub issues and pull requests.

Cross-domain links such as `ticket -> implemented_by -> pull request -> modifies
-> code symbol` belong to model packs and reconciliation rules, not special cases
inside the orchestration engine.
