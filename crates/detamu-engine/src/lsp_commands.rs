//! `detamu lsp` registration commands.

use std::{path::Path, process::ExitCode};

use detamu_language_lsp::LspRegistration;
use serde_json::{Value, json};

use crate::lsp_registry::LspRegistry;

pub fn run(subcommand: Option<&str>, arguments: impl Iterator<Item = String>) -> ExitCode {
    match subcommand {
        Some("register") => register(arguments),
        Some("unregister") => unregister(arguments),
        Some("list") => list(arguments),
        _ => {
            eprintln!("{}", usage());
            ExitCode::from(2)
        }
    }
}

fn register(arguments: impl Iterator<Item = String>) -> ExitCode {
    let parsed = match parse(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return failure(&error),
    };
    if parsed.positionals.len() != 1 || parsed.id.is_none() {
        return failure(usage());
    }
    let registration = LspRegistration {
        id: parsed.id.unwrap_or_default(),
        language: parsed.language.unwrap_or_default(),
        command: parsed.command.unwrap_or_default(),
        arguments: parsed.arguments,
        extensions: parsed.extensions,
        initialization_options: parsed.initialization,
    };
    let mut registry = match LspRegistry::load(Path::new(&parsed.positionals[0])) {
        Ok(registry) => registry,
        Err(error) => return failure(&error),
    };
    match registry.upsert(registration) {
        Ok(registration) => success(&json!(registration)),
        Err(error) => failure(&error),
    }
}

fn unregister(arguments: impl Iterator<Item = String>) -> ExitCode {
    let parsed = match parse(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return failure(&error),
    };
    let Some(id) = parsed.id else {
        return failure(usage());
    };
    if parsed.positionals.len() != 1 {
        return failure(usage());
    }
    let mut registry = match LspRegistry::load(Path::new(&parsed.positionals[0])) {
        Ok(registry) => registry,
        Err(error) => return failure(&error),
    };
    match registry.remove(&id) {
        Ok(removed) => success(&json!({ "id": id, "removed": removed })),
        Err(error) => failure(&error),
    }
}

fn list(arguments: impl Iterator<Item = String>) -> ExitCode {
    let parsed = match parse(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return failure(&error),
    };
    if parsed.positionals.len() != 1 {
        return failure(usage());
    }
    match LspRegistry::load(Path::new(&parsed.positionals[0])) {
        Ok(registry) => success(&json!(registry.registrations())),
        Err(error) => failure(&error),
    }
}

struct Parsed {
    positionals: Vec<String>,
    id: Option<String>,
    language: Option<String>,
    command: Option<String>,
    arguments: Vec<String>,
    extensions: Vec<String>,
    initialization: Option<Value>,
}

fn parse(arguments: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        positionals: Vec::new(),
        id: None,
        language: None,
        command: None,
        arguments: Vec::new(),
        extensions: Vec::new(),
        initialization: None,
    };
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments
                .next()
                .ok_or_else(|| format!("--{name} requires a value"))
        };
        match argument.as_str() {
            "--id" => parsed.id = Some(value("id")?),
            "--language" => parsed.language = Some(value("language")?),
            "--command" => parsed.command = Some(value("command")?),
            "--arg" => parsed.arguments.push(value("arg")?),
            "--ext" => parsed.extensions.push(value("ext")?),
            "--init" => {
                let text = value("init")?;
                parsed.initialization = Some(
                    serde_json::from_str(&text)
                        .map_err(|error| format!("--init must be JSON: {error}"))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other}\n{}", usage()));
            }
            other => parsed.positionals.push(other.to_owned()),
        }
    }
    Ok(parsed)
}

fn success(data: &Value) -> ExitCode {
    println!(
        "{}",
        json!({
            "schema_version": 1,
            "kind": "lsp",
            "data": data,
        })
    );
    ExitCode::SUCCESS
}

fn failure(message: &str) -> ExitCode {
    eprintln!(
        "{}",
        json!({
            "schema_version": 1,
            "kind": "error",
            "command": "lsp",
            "error": message,
        })
    );
    ExitCode::from(2)
}

fn usage() -> &'static str {
    "usage: detamu lsp register <DATABASE_PATH> --id <ID> --language <LANGUAGE> --command <COMMAND> [--arg <ARG>]... [--ext <EXT>]... [--init <JSON>]\n       \
     detamu lsp unregister <DATABASE_PATH> --id <ID>\n       \
     detamu lsp list <DATABASE_PATH>"
}
