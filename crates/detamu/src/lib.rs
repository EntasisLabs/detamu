//! The public Detamu facade.
//!
//! Foundational orchestration and storage contracts are always available.
//! Query, code-domain, runtime-discovery, and `SurrealDB` integrations are exposed
//! through additive Cargo features.

pub use detamu_core as core;
pub use detamu_model as model;
pub use detamu_sdk as sdk;
pub use detamu_store as store;

#[cfg(feature = "query")]
pub use detamu_query as query;

#[cfg(feature = "runtime")]
pub use detamu_runtime as runtime;

#[cfg(feature = "code")]
pub use detamu_model_code as code;

#[cfg(feature = "code")]
pub use detamu_query_code as code_query;

#[cfg(feature = "surreal")]
pub use detamu_surreal as surreal;
