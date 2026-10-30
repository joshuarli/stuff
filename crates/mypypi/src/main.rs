#![forbid(unsafe_code)]

use mypypi::cli;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // Help handling mirrors argparse: --help anywhere prints help and exits 0.
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        let command = argv.iter().find_map(|a| match a.as_str() {
            "add" => Some(cli::Command::Add),
            "plan" => Some(cli::Command::Plan),
            "build" => Some(cli::Command::Build),
            "validate" => Some(cli::Command::Validate),
            "publish" => Some(cli::Command::Publish),
            _ => None,
        });
        match command {
            Some(c) => cli::print_command_help(&c),
            None => cli::print_help(),
        }
        return;
    }
    if argv.is_empty() {
        cli::print_help();
        return;
    }
    let parsed = match cli::parse_args(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("mypypi: {e}");
            std::process::exit(2);
        }
    };
    match cli::run(&parsed) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let settings = parsed
                .config
                .as_deref()
                .map(std::path::PathBuf::from)
                .and_then(|p| mypypi::config::load_project_settings(&p).ok());
            eprintln!("{}", cli::format_error(&e, settings.as_ref()));
            std::process::exit(e.exit_code());
        }
    }
}
