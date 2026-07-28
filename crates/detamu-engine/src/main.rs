use std::process::ExitCode;

use detamu_surreal::SurrealStore;

#[tokio::main]
async fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next();
    match command.as_deref() {
        Some("version" | "--version" | "-V") => {
            println!("detamu {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("doctor") => {
            let report = serde_json::json!({
                "name": "detamu",
                "version": env!("CARGO_PKG_VERSION"),
                "sdk": "available",
                "store": "in-memory",
                "surreal": "surrealkv",
                "world_models": ["code"],
                "language_packs": [],
            });
            println!("{report}");
            ExitCode::SUCCESS
        }
        Some("init") => {
            let Some(path) = arguments.next() else {
                eprintln!("usage: detamu init <PATH> [NAMESPACE] [DATABASE]");
                return ExitCode::from(2);
            };
            let namespace = arguments.next().unwrap_or_else(|| "detamu".to_owned());
            let database = arguments.next().unwrap_or_else(|| "detamu".to_owned());
            match SurrealStore::surrealkv(&path, &namespace, &database).await {
                Ok(_) => {
                    println!("initialized Detamu SurrealKV at {path}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("failed to initialize Detamu SurrealKV: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("help" | "--help" | "-h") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(unknown) => {
            eprintln!("unknown command: {unknown}\n");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!(
        "Detamu — a versioned world-model engine\n\n\
         Usage: detamu <COMMAND>\n\n\
         Commands:\n  \
           doctor    Report installed engine capabilities\n  \
           init      Initialize a native SurrealKV database\n  \
           version   Print the engine version\n  \
           help      Print this help"
    );
}
