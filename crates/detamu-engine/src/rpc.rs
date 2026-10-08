//! TCP host for the shared JSON-RPC handler.

use std::sync::Arc;

use detamu_rpc::CodeRouter;
use detamu_sdk::{Detamu, TcpListenerAdapter};
use detamu_store::DetamuStore;
use detamu_surreal::SurrealStore;
use serde_json::json;

/// Listens on `bind:port` and serves one JSON-RPC exchange per connection.
///
/// # Errors
///
/// Returns an error when the database cannot be opened or the socket cannot be bound.
pub async fn serve(
    database: &str,
    namespace: &str,
    name: &str,
    bind: &str,
    port: u16,
) -> Result<(), String> {
    let listener = TcpListenerAdapter::bind((bind, port))
        .await
        .map_err(|error| format!("listen on {bind}:{port}: {error}"))?;
    let store = SurrealStore::surrealkv(database, namespace, name)
        .await
        .map(|store| Arc::new(store) as Arc<dyn DetamuStore>)
        .map_err(|error| format!("open Detamu SurrealKV: {error}"))?;
    println!(
        "{}",
        json!({
            "schema_version": 1,
            "kind": "serve",
            "data": { "bind": bind, "port": port },
        })
    );
    let router = CodeRouter::new(Arc::clone(&store), database);
    Detamu::builder(store)
        .with_listener(listener)
        .build()
        .serve(router)
        .await
        .map_err(|error| error.to_string())
}
