//! JSON-RPC handler for code queries and language-server registration.
//!
//! Pass [`CodeRouter`] to [`detamu_sdk::Detamu::serve`] after registering a
//! stream adapter with [`detamu_sdk::DetamuBuilder::with_listener`].

mod protocol;

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use detamu_sdk::LineHandler;
use detamu_store::DetamuStore;

pub use protocol::respond;

/// Routes newline-delimited JSON-RPC onto a Detamu store.
pub struct CodeRouter {
    store: Arc<dyn DetamuStore>,
    database: PathBuf,
}

impl CodeRouter {
    /// Handles `detamu.*` and `acc.*` methods against `store`.
    ///
    /// `database` is the `SurrealKV` path. Language-server registrations are
    /// stored beside it and picked up by the next index.
    pub fn new(store: Arc<dyn DetamuStore>, database: impl Into<PathBuf>) -> Self {
        Self {
            store,
            database: database.into(),
        }
    }
}

#[async_trait]
impl LineHandler for CodeRouter {
    async fn handle_line(&self, line: String) -> String {
        respond(Arc::clone(&self.store), &self.database, &line).await
    }
}
