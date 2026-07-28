# Guidance for coding agents

- Preserve the SDK/engine separation: the engine hosts the SDK.
- Keep `detamu-core` free of database, process, and consumer dependencies.
- Bind persisted analysis to immutable revision identifiers, not branch names.
- Add analyzers through normalized observations; do not leak provider-specific
  payloads into the domain model.
- Represent unavailable or partial analysis explicitly.
- Prefer bulk persistence over per-node or per-edge round trips.
- Version scoring behavior and serialized contracts before changing semantics.
- Run formatting, clippy with warnings denied, and workspace tests before handoff.

