use std::{collections::BTreeMap, process::ExitCode, sync::Arc};

use detamu_core::{EntityId, SnapshotId, SnapshotVersion, WorldId};
use detamu_query::{QUERY_SCHEMA_VERSION, SnapshotQuery};
use detamu_query_code::{CodeEntityFilter, CodeQuery};
use detamu_store::DetamuStore;
use detamu_surreal::SurrealStore;
use serde_json::{Value, json};

pub async fn run(command: &str, arguments: impl Iterator<Item = String>) -> ExitCode {
    let parsed = match Arguments::parse(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return failure(command, &error, 2),
    };
    let result = match command {
        "snapshots" => snapshots(parsed).await,
        "inspect" => inspect(parsed).await,
        "find" => find(parsed).await,
        "impact" => impact(parsed).await,
        "diff" => diff(parsed).await,
        "gaps" => gaps(parsed).await,
        _ => unreachable!("query command is validated by main"),
    };
    match result {
        Ok(value) => success(command, &value),
        Err(error) => failure(command, &error, 1),
    }
}

async fn snapshots(arguments: Arguments) -> Result<Value, String> {
    arguments.require_positionals(1, snapshots_usage)?;
    arguments.allow_options(&["world", "namespace", "database"], snapshots_usage)?;
    let connection = arguments.connection()?;
    let world = arguments.option("world")?.map(WorldId::new);
    let query = SnapshotQuery::new(open_store(&arguments.positionals[0], &connection).await?);
    let snapshots = query
        .snapshots(world.as_ref())
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(snapshots).map_err(|error| error.to_string())
}

async fn inspect(arguments: Arguments) -> Result<Value, String> {
    arguments.require_positionals(4, inspect_usage)?;
    arguments.allow_options(&["namespace", "database"], inspect_usage)?;
    let connection = arguments.connection()?;
    let snapshot = arguments.snapshot(1, 2);
    let entity = EntityId::new(&arguments.positionals[3]);
    let store = open_store(&arguments.positionals[0], &connection).await?;
    let query = SnapshotQuery::new(store.clone());
    let observation = query
        .entity(&snapshot, &entity)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("entity {entity} does not exist in snapshot"))?;
    let relations = store
        .relations(&snapshot, &entity, detamu_store::RelationDirection::Both)
        .await
        .map_err(|error| error.to_string())?;
    Ok(json!({ "entity": observation, "relations": relations }))
}

async fn find(arguments: Arguments) -> Result<Value, String> {
    arguments.require_positionals(3, find_usage)?;
    arguments.allow_options(
        &[
            "path",
            "name",
            "kind",
            "language",
            "line",
            "limit",
            "namespace",
            "database",
        ],
        find_usage,
    )?;
    let connection = arguments.connection()?;
    let snapshot = arguments.snapshot(1, 2);
    let query = CodeQuery::new(open_store(&arguments.positionals[0], &connection).await?);
    let path = arguments.option("path")?;
    if let Some(line) = arguments.parse_option::<u32>("line")? {
        let path = path.ok_or_else(|| "--line requires --path".to_owned())?;
        let entity = query
            .at_location(&snapshot, path, line)
            .await
            .map_err(|error| error.to_string())?;
        return serde_json::to_value(entity.into_iter().collect::<Vec<_>>())
            .map_err(|error| error.to_string());
    }
    let entities = query
        .find(
            &snapshot,
            &CodeEntityFilter {
                path: path.map(str::to_owned),
                name_contains: arguments.option("name")?.map(str::to_owned),
                kind: arguments.option("kind")?.map(str::to_owned),
                language: arguments.option("language")?.map(str::to_owned),
                limit: arguments.parse_option("limit")?,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(entities).map_err(|error| error.to_string())
}

async fn impact(arguments: Arguments) -> Result<Value, String> {
    arguments.require_positionals(4, impact_usage)?;
    arguments.allow_options(
        &["depth", "max-nodes", "namespace", "database"],
        impact_usage,
    )?;
    let connection = arguments.connection()?;
    let snapshot = arguments.snapshot(1, 2);
    let query = CodeQuery::new(open_store(&arguments.positionals[0], &connection).await?);
    let impact = query
        .impact(
            &snapshot,
            &EntityId::new(&arguments.positionals[3]),
            arguments.parse_option("depth")?.unwrap_or(3),
            arguments.parse_option("max-nodes")?.unwrap_or(1_000),
        )
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(impact).map_err(|error| error.to_string())
}

async fn diff(arguments: Arguments) -> Result<Value, String> {
    arguments.require_positionals(4, diff_usage)?;
    arguments.allow_options(&["namespace", "database"], diff_usage)?;
    let connection = arguments.connection()?;
    let world = WorldId::new(&arguments.positionals[1]);
    let from = SnapshotId::new(
        world.clone(),
        SnapshotVersion::new(&arguments.positionals[2]),
    );
    let to = SnapshotId::new(world, SnapshotVersion::new(&arguments.positionals[3]));
    let query = SnapshotQuery::new(open_store(&arguments.positionals[0], &connection).await?);
    let diff = query
        .diff(&from, &to)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(diff).map_err(|error| error.to_string())
}

async fn gaps(arguments: Arguments) -> Result<Value, String> {
    arguments.require_positionals(3, gaps_usage)?;
    arguments.allow_options(&["namespace", "database"], gaps_usage)?;
    let connection = arguments.connection()?;
    let snapshot = arguments.snapshot(1, 2);
    let query = CodeQuery::new(open_store(&arguments.positionals[0], &connection).await?);
    let gaps = query
        .gaps(&snapshot)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(gaps).map_err(|error| error.to_string())
}

async fn open_store(path: &str, connection: &Connection) -> Result<Arc<dyn DetamuStore>, String> {
    SurrealStore::surrealkv(path, &connection.namespace, &connection.database)
        .await
        .map(|store| Arc::new(store) as Arc<dyn DetamuStore>)
        .map_err(|error| format!("failed to open Detamu SurrealKV: {error}"))
}

fn success(command: &str, data: &Value) -> ExitCode {
    println!(
        "{}",
        json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "kind": command,
            "data": data,
        })
    );
    ExitCode::SUCCESS
}

fn failure(command: &str, message: &str, code: u8) -> ExitCode {
    eprintln!(
        "{}",
        json!({
            "schema_version": QUERY_SCHEMA_VERSION,
            "kind": "error",
            "command": command,
            "error": message,
        })
    );
    ExitCode::from(code)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Connection {
    namespace: String,
    database: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Arguments {
    positionals: Vec<String>,
    options: BTreeMap<String, Vec<String>>,
}

impl Arguments {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut positionals = Vec::new();
        let mut options = BTreeMap::<String, Vec<String>>::new();
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            if let Some(name) = argument.strip_prefix("--") {
                if name.is_empty() {
                    return Err("option name cannot be empty".to_owned());
                }
                let value = arguments
                    .next()
                    .ok_or_else(|| format!("--{name} requires a value"))?;
                options.entry(name.to_owned()).or_default().push(value);
            } else {
                positionals.push(argument);
            }
        }
        Ok(Self {
            positionals,
            options,
        })
    }

    fn require_positionals(
        &self,
        expected: usize,
        usage: fn() -> &'static str,
    ) -> Result<(), String> {
        if self.positionals.len() == expected {
            Ok(())
        } else {
            Err(usage().to_owned())
        }
    }

    fn allow_options(&self, allowed: &[&str], usage: fn() -> &'static str) -> Result<(), String> {
        if let Some(name) = self
            .options
            .keys()
            .find(|name| !allowed.contains(&name.as_str()))
        {
            return Err(format!("unknown option --{name}\n{}", usage()));
        }
        if let Some(name) = self
            .options
            .iter()
            .find_map(|(name, values)| (values.len() > 1).then_some(name))
        {
            return Err(format!("option --{name} may only be supplied once"));
        }
        Ok(())
    }

    fn option(&self, name: &str) -> Result<Option<&str>, String> {
        let values = self.options.get(name);
        if values.is_some_and(|values| values.len() > 1) {
            return Err(format!("option --{name} may only be supplied once"));
        }
        Ok(values.and_then(|values| values.first()).map(String::as_str))
    }

    fn parse_option<T>(&self, name: &str) -> Result<Option<T>, String>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        self.option(name)?
            .map(|value| {
                value
                    .parse()
                    .map_err(|error| format!("invalid --{name}: {error}"))
            })
            .transpose()
    }

    fn connection(&self) -> Result<Connection, String> {
        Ok(Connection {
            namespace: self.option("namespace")?.unwrap_or("detamu").to_owned(),
            database: self.option("database")?.unwrap_or("detamu").to_owned(),
        })
    }

    fn snapshot(&self, world: usize, version: usize) -> SnapshotId {
        SnapshotId::new(
            WorldId::new(&self.positionals[world]),
            SnapshotVersion::new(&self.positionals[version]),
        )
    }
}

fn snapshots_usage() -> &'static str {
    "usage: detamu snapshots <DATABASE_PATH> [--world <WORLD>] [--namespace <NS>] [--database <DB>]"
}

fn inspect_usage() -> &'static str {
    "usage: detamu inspect <DATABASE_PATH> <WORLD> <SNAPSHOT> <ENTITY_ID> [--namespace <NS>] [--database <DB>]"
}

fn find_usage() -> &'static str {
    "usage: detamu find <DATABASE_PATH> <WORLD> <SNAPSHOT> [--path <PATH>] [--line <LINE>] [--name <TEXT>] [--kind <KIND>] [--language <LANGUAGE>] [--limit <COUNT>]"
}

fn impact_usage() -> &'static str {
    "usage: detamu impact <DATABASE_PATH> <WORLD> <SNAPSHOT> <ENTITY_ID> [--depth <DEPTH>] [--max-nodes <COUNT>]"
}

fn diff_usage() -> &'static str {
    "usage: detamu diff <DATABASE_PATH> <WORLD> <FROM_SNAPSHOT> <TO_SNAPSHOT> [--namespace <NS>] [--database <DB>]"
}

fn gaps_usage() -> &'static str {
    "usage: detamu gaps <DATABASE_PATH> <WORLD> <SNAPSHOT> [--namespace <NS>] [--database <DB>]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_options_independently_of_positionals() {
        let arguments = Arguments::parse(
            [
                "db",
                "world",
                "snapshot",
                "--path",
                "src/lib.rs",
                "--limit",
                "5",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("parse arguments");

        assert_eq!(arguments.positionals, ["db", "world", "snapshot"]);
        assert_eq!(arguments.option("path").expect("path"), Some("src/lib.rs"));
        assert_eq!(
            arguments.parse_option::<usize>("limit").expect("limit"),
            Some(5)
        );
    }

    #[test]
    fn rejects_duplicate_single_value_options() {
        let arguments = Arguments::parse(
            ["--depth", "2", "--depth", "3"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("parse arguments");

        assert!(arguments.option("depth").is_err());
    }
}
