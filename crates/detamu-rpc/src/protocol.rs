//! Newline-delimited JSON-RPC for code queries and language-server registration.
//!
//! Methods accept a `detamu.` or `acc.` prefix. The SDK listener writes each
//! returned string as one response line.

use std::{path::Path, sync::Arc};

use detamu_core::{EntityId, SnapshotId, SnapshotVersion, WorldId};
use detamu_language_lsp::{LspRegistration, LspRegistry};
use detamu_model_code::AvecScores;
use detamu_query::SnapshotQuery;
use detamu_query_code::{
    CodeEntityFilter, CodeQuery, FRICTION_MINIMUM, PATTERN_LIMIT, PATTERN_THRESHOLD, RANK_LIMIT,
    UNSTABLE_MAXIMUM,
};
use detamu_store::{DetamuStore, RelationDirection};
use serde_json::{Value, json};

pub async fn respond(store: Arc<dyn DetamuStore>, database: &Path, line: &str) -> String {
    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return error_response(&Value::Null, -32700, "parse error"),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return error_response(&id, -32600, "invalid request");
    };
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    match dispatch(store, database, method, &params).await {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        Err(failure) => error_response(&id, failure.code, &failure.message),
    }
}

struct Failure {
    code: i32,
    message: String,
}

impl Failure {
    fn params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }

    fn missing() -> Self {
        Self {
            code: -32601,
            message: "method not found".to_owned(),
        }
    }
}

async fn dispatch(
    store: Arc<dyn DetamuStore>,
    database: &Path,
    method: &str,
    params: &Value,
) -> Result<Value, Failure> {
    let name = method
        .strip_prefix("detamu.")
        .or_else(|| method.strip_prefix("acc."))
        .unwrap_or(method);
    if matches!(
        name,
        "registerLsp"
            | "registerLspStream"
            | "unregisterLsp"
            | "unregisterLspStream"
            | "listLsp"
            | "listLspStreams"
    ) {
        return dispatch_lsp(database, name, params);
    }
    dispatch_query(store, name, params).await
}

fn dispatch_lsp(database: &Path, name: &str, params: &Value) -> Result<Value, Failure> {
    match name {
        "registerLsp" | "registerLspStream" => {
            let registration = registration_param(params)?;
            let mut registry = LspRegistry::load(database).map_err(internal)?;
            let stored = registry.upsert(registration).map_err(Failure::params)?;
            Ok(json!({ "success": true, "id": stored.id, "streamId": stored.id }))
        }
        "unregisterLsp" | "unregisterLspStream" => {
            let id = params
                .get("id")
                .or_else(|| params.get("streamId"))
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::params("id is required"))?;
            let mut registry = LspRegistry::load(database).map_err(internal)?;
            let removed = registry.remove(id).map_err(internal)?;
            Ok(json!(removed))
        }
        "listLsp" | "listLspStreams" => {
            let registry = LspRegistry::load(database).map_err(internal)?;
            serde_json::to_value(registry.registrations()).map_err(internal)
        }
        _ => Err(Failure::missing()),
    }
}

async fn dispatch_query(
    store: Arc<dyn DetamuStore>,
    name: &str,
    params: &Value,
) -> Result<Value, Failure> {
    let query = CodeQuery::new(store);
    match name {
        "getNode" => {
            let snapshot = resolve_snapshot(query.generic().store(), params).await?;
            let node = query
                .node(&snapshot, &node_id(params)?, true)
                .await
                .map_err(internal)?;
            serde_json::to_value(node).map_err(internal)
        }
        "queryRelations" => {
            let snapshot = resolve_snapshot(query.generic().store(), params).await?;
            let include_scores = bool_param(params, "includeScores", false);
            let node = query
                .node(&snapshot, &node_id(params)?, include_scores)
                .await
                .map_err(internal)?;
            serde_json::to_value(node).map_err(internal)
        }
        "queryDependencies" => {
            let snapshot = resolve_snapshot(query.generic().store(), params).await?;
            let direction = direction_param(params.get("direction").and_then(Value::as_str))?;
            let max_depth = depth_param(params.get("maxDepth"))?;
            let max_nodes = usize_param(params, "maxNodes", 10_000)?;
            let include_scores = bool_param(params, "includeScores", false);
            let dependencies = query
                .dependencies(
                    &snapshot,
                    &node_id(params)?,
                    direction,
                    max_depth,
                    max_nodes,
                    include_scores,
                )
                .await
                .map_err(internal)?;
            serde_json::to_value(dependencies).map_err(internal)
        }
        "queryPatterns" => {
            let snapshot = resolve_snapshot(query.generic().store(), params).await?;
            let profile = profile_param(params)?;
            let threshold = f64_param(params, "threshold", PATTERN_THRESHOLD)?;
            let limit = usize_param(params, "limit", PATTERN_LIMIT)?;
            let matches = query
                .patterns(&snapshot, profile, threshold, limit)
                .await
                .map_err(internal)?;
            serde_json::to_value(matches).map_err(internal)
        }
        "search" | "getHighFriction" | "getUnstable" | "getStats" => {
            dispatch_ranked(query, name, params).await
        }
        _ => Err(Failure::missing()),
    }
}

async fn dispatch_ranked(query: CodeQuery, name: &str, params: &Value) -> Result<Value, Failure> {
    let snapshot = resolve_snapshot(query.generic().store(), params).await?;
    match name {
        "search" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::params("name is required"))?;
            let limit = usize_param(params, "limit", 10)?;
            let entities = query
                .find(
                    &snapshot,
                    &CodeEntityFilter {
                        name_contains: Some(name.to_owned()),
                        limit: Some(limit),
                        ..CodeEntityFilter::default()
                    },
                )
                .await
                .map_err(internal)?;
            let include_scores = bool_param(params, "includeScores", true);
            let nodes = entities
                .iter()
                .map(|entity| CodeQuery::describe(entity, include_scores))
                .collect::<Vec<_>>();
            serde_json::to_value(nodes).map_err(internal)
        }
        "getHighFriction" => {
            let minimum = f64_param(params, "minFriction", FRICTION_MINIMUM)?;
            let limit = usize_param(params, "limit", RANK_LIMIT)?;
            let nodes = query
                .high_friction(&snapshot, minimum, limit)
                .await
                .map_err(internal)?;
            serde_json::to_value(nodes).map_err(internal)
        }
        "getUnstable" => {
            let maximum = f64_param(params, "maxStability", UNSTABLE_MAXIMUM)?;
            let limit = usize_param(params, "limit", RANK_LIMIT)?;
            let nodes = query
                .unstable(&snapshot, maximum, limit)
                .await
                .map_err(internal)?;
            serde_json::to_value(nodes).map_err(internal)
        }
        "getStats" => {
            let stats = query.stats(&snapshot).await.map_err(internal)?;
            serde_json::to_value(stats).map_err(internal)
        }
        _ => Err(Failure::missing()),
    }
}

fn internal<E: std::fmt::Display>(error: E) -> Failure {
    Failure::internal(error.to_string())
}

async fn resolve_snapshot(
    store: &Arc<dyn DetamuStore>,
    params: &Value,
) -> Result<SnapshotId, Failure> {
    let world = params.get("world").and_then(Value::as_str);
    let version = params.get("snapshot").and_then(Value::as_str);
    if let (Some(world), Some(version)) = (world, version) {
        return Ok(SnapshotId::new(
            WorldId::new(world),
            SnapshotVersion::new(version),
        ));
    }
    if version.is_some() && world.is_none() {
        return Err(Failure::params(
            "snapshot requires world when more than one world is stored",
        ));
    }
    let world = world.map(WorldId::new);
    let snapshots = SnapshotQuery::new(Arc::clone(store))
        .snapshots(world.as_ref())
        .await
        .map_err(internal)?;
    match snapshots.as_slice() {
        [snapshot] => Ok(snapshot.snapshot.clone()),
        [] => Err(Failure::params("no snapshots are stored")),
        _ => Err(Failure::params(
            "world and snapshot are required when more than one snapshot is stored",
        )),
    }
}

fn node_id(params: &Value) -> Result<EntityId, Failure> {
    params
        .get("nodeId")
        .or_else(|| params.get("entity"))
        .and_then(Value::as_str)
        .map(EntityId::new)
        .ok_or_else(|| Failure::params("nodeId is required"))
}

fn direction_param(value: Option<&str>) -> Result<RelationDirection, Failure> {
    match value.unwrap_or("both").to_ascii_lowercase().as_str() {
        "incoming" => Ok(RelationDirection::Incoming),
        "outgoing" => Ok(RelationDirection::Outgoing),
        "both" => Ok(RelationDirection::Both),
        other => Err(Failure::params(format!(
            "direction must be incoming, outgoing, or both, not {other}"
        ))),
    }
}

fn depth_param(value: Option<&Value>) -> Result<u32, Failure> {
    let Some(value) = value else {
        return Ok(u32::MAX);
    };
    let depth = value
        .as_i64()
        .ok_or_else(|| Failure::params("maxDepth must be an integer"))?;
    if depth < 0 {
        Ok(u32::MAX)
    } else {
        u32::try_from(depth).map_err(|_| Failure::params("maxDepth is too large"))
    }
}

fn bool_param(params: &Value, name: &str, default: bool) -> bool {
    params.get(name).and_then(Value::as_bool).unwrap_or(default)
}

fn f64_param(params: &Value, name: &str, default: f64) -> Result<f64, Failure> {
    match params.get(name) {
        Some(value) => value
            .as_f64()
            .ok_or_else(|| Failure::params(format!("{name} must be a number"))),
        None => Ok(default),
    }
}

fn usize_param(params: &Value, name: &str, default: usize) -> Result<usize, Failure> {
    match params.get(name) {
        Some(value) => {
            let number = value
                .as_u64()
                .ok_or_else(|| Failure::params(format!("{name} must be a positive integer")))?;
            usize::try_from(number).map_err(|_| Failure::params(format!("{name} is too large")))
        }
        None => Ok(default),
    }
}

fn profile_param(params: &Value) -> Result<AvecScores, Failure> {
    let source = params.get("profile").unwrap_or(params);
    Ok(AvecScores {
        stability: required_f64(source, "stability")?,
        logic: required_f64(source, "logic")?,
        friction: required_f64(source, "friction")?,
        autonomy: required_f64(source, "autonomy")?,
    })
}

fn required_f64(params: &Value, name: &str) -> Result<f64, Failure> {
    params
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| Failure::params(format!("{name} is required")))
}

fn registration_param(params: &Value) -> Result<LspRegistration, Failure> {
    let command = params
        .get("command")
        .or_else(|| params.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Failure::params("command is required; Detamu launches the language server over stdio")
        })?;
    if params.get("command").is_none() && params.get("port").is_some() {
        return Err(Failure::params(
            "TCP language-server ports are not a registration target; pass the stdio command instead",
        ));
    }
    let extensions = params
        .get("extensions")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .or_else(|| {
            params
                .get("extension")
                .and_then(Value::as_str)
                .map(|extension| vec![extension.to_owned()])
        })
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(LspRegistration {
        id: params
            .get("id")
            .or_else(|| params.get("language"))
            .and_then(Value::as_str)
            .unwrap_or("lsp")
            .to_owned(),
        language: params
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        command: command.to_owned(),
        arguments,
        extensions,
        initialization_options: params.get("initializationOptions").cloned(),
    })
}

fn error_response(id: &Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use detamu_core::{Entity, EntityObservation, ModelId, ObservationBatch, Score, ScoreModelId};
    use detamu_store::InMemoryStore;

    use super::*;

    #[tokio::test]
    async fn stats_and_registration_round_trip_over_json_rpc() {
        let directory = std::env::temp_dir().join(format!("detamu-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("temp dir");
        let database = directory.join("db");
        let store = Arc::new(InMemoryStore::default());
        let snapshot = SnapshotId::new(
            WorldId::new("code.repository:fixture"),
            SnapshotVersion::new("v1"),
        );
        let mut entity = EntityObservation {
            snapshot: snapshot.clone(),
            entity: Entity {
                id: EntityId::new("node"),
                model: ModelId::new("code"),
                kind: "function".to_owned(),
                label: "parse".to_owned(),
            },
            attributes: BTreeMap::default(),
            measurements: Vec::new(),
            scores: vec![
                score("stability", 0.2),
                score("logic", 0.3),
                score("friction", 0.9),
                score("autonomy", 0.4),
            ],
        };
        entity
            .attributes
            .insert("language".to_owned(), json!("python"));
        let mut batch = ObservationBatch::empty(snapshot);
        batch.entities.push(entity);
        store.ingest(batch).await.expect("ingest");

        let stats = respond(
            Arc::clone(&store) as Arc<dyn DetamuStore>,
            &database,
            r#"{"jsonrpc":"2.0","id":1,"method":"acc.getStats","params":{}}"#,
        )
        .await;
        let stats: Value = serde_json::from_str(&stats).expect("stats json");
        assert_eq!(stats["result"]["scored_entities"], 1);
        assert_eq!(stats["id"], 1);

        let registered = respond(
            store,
            &database,
            r#"{"jsonrpc":"2.0","id":2,"method":"detamu.registerLsp","params":{"id":"python","language":"python","command":"pyright-langserver","arguments":["--stdio"],"extensions":["py"]}}"#,
        )
        .await;
        let registered: Value = serde_json::from_str(&registered).expect("register json");
        assert_eq!(registered["result"]["success"], true);
        assert_eq!(
            LspRegistry::load(&database)
                .expect("registry")
                .registrations()[0]
                .id,
            "python"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn score(dimension: &str, value: f64) -> Score {
        Score {
            model: ScoreModelId::new("avec.code"),
            version: 1,
            dimension: dimension.to_owned(),
            value,
        }
    }
}
