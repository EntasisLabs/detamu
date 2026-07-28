use std::process::ExitCode;

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
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
                "surreal": "not-installed",
                "language_packs": [],
            });
            println!("{report}");
            ExitCode::SUCCESS
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
        "Detamu — a living reference model for codebases\n\n\
         Usage: detamu <COMMAND>\n\n\
         Commands:\n  \
           doctor    Report installed engine capabilities\n  \
           version   Print the engine version\n  \
           help      Print this help"
    );
}
