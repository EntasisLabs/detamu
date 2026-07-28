# Querying Detamu

Detamu exposes the same persisted world through two consumption surfaces:

- `detamu-query` and `detamu-query-code` for embedded Rust clients;
- JSON commands from `detamu-engine` for Medousa and non-Rust clients.

The generic query layer knows snapshots, entities, relations, traversal, and
comparison. Code meaning remains in the code facade.

## Snapshot identity

Every query addresses an immutable `WorldId` and source version. Indexing without
`--revision` resolves `HEAD`; historical or reproducible indexing can name any Git
commit-ish:

```bash
detamu-engine index . ./data/detamu.surrealkv --revision d0cfd57
detamu-engine index . ./data/detamu.surrealkv --revision 51d3ada
```

Both commits coexist as snapshots. Re-indexing the same identity atomically
replaces only that snapshot.

## JSON protocol

Successful commands write one JSON value to stdout:

```json
{"schema_version":1,"kind":"snapshots","data":[]}
```

Failures write a stable error envelope to stderr and exit nonzero:

```json
{"schema_version":1,"kind":"error","command":"inspect","error":"..."}
```

`schema_version` versions the outer protocol. Domain payloads retain their own
model and scoring versions.

```text
detamu-engine snapshots DB [--world WORLD]
detamu-engine inspect DB WORLD SNAPSHOT ENTITY
detamu-engine find DB WORLD SNAPSHOT [--path PATH] [--line LINE]
    [--name NAME] [--kind KIND] [--language LANGUAGE] [--limit N]
detamu-engine impact DB WORLD SNAPSHOT ENTITY [--depth N] [--max-nodes N]
detamu-engine diff DB WORLD FROM_SNAPSHOT TO_SNAPSHOT
detamu-engine gaps DB WORLD SNAPSHOT
```

All commands also accept `--namespace` and `--database`; both default to
`detamu`.

`find --line` uses one-based source lines and returns the narrowest matching code
entity. `impact` follows incoming call, reference, import, implementation, and
inheritance edges to answer which code depends on the target. Traversal is
cycle-safe and explicitly reports truncation when `--max-nodes` is reached.

`diff` compares observation content rather than snapshot identity, so an entity
copied unchanged into a later commit is not reported as changed. `gaps` reports
which scoreable code entities lack each required AVEC measurement or score; it
does not reinterpret missing semantic or coverage evidence as zero.

## Rust SDK

Both facades accept `Arc<dyn DetamuStore>`, so embedded clients can use the
in-memory reference implementation, SurrealDB memory storage, or persistent
SurrealKV without changing query code.

```rust,no_run
use std::sync::Arc;
use detamu_query::{EntityFilter, SnapshotQuery};
use detamu_store::DetamuStore;

async fn list(store: Arc<dyn DetamuStore>) -> Result<(), detamu_query::QueryError> {
    let query = SnapshotQuery::new(store);
    let entities = query.find(&snapshot_id(), &EntityFilter::default()).await?;
    println!("{}", entities.len());
    Ok(())
# }
# fn snapshot_id() -> detamu_core::SnapshotId { todo!() }
```

Code-aware clients wrap the same store:

```rust,no_run
use std::sync::Arc;
use detamu_query_code::{CodeEntityFilter, CodeQuery};
use detamu_store::DetamuStore;

async fn locate(
    store: Arc<dyn DetamuStore>,
) -> Result<(), detamu_query::QueryError> {
    let query = CodeQuery::new(store);
    let filter = CodeEntityFilter {
        path: Some("src/main.rs".into()),
        line: Some(42),
        ..Default::default()
    };
    let entities = query.find(&snapshot_id(), &filter).await?;
    println!("{}", entities.len());
    Ok(())
# }
# fn snapshot_id() -> detamu_core::SnapshotId { todo!() }
```

The engine JSON surface deliberately mirrors these operations, but it is a thin
protocol host rather than a second query implementation.
