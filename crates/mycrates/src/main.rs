#![forbid(unsafe_code)]

use mycrates::cli;
use std::process;

fn main() {
    let args = match cli::parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            process::exit(e.exit_code());
        }
    };

    let res = match args.command {
        cli::Command::Init => mycrates::cli::init::run(&args.args),
        cli::Command::Plan => mycrates::cli::plan::run(&args.args),
        cli::Command::ImportLock => mycrates::cli::import_lock::run(&args.args),
        cli::Command::Add => mycrates::cli::add::run(&args.args),
        cli::Command::Fetch => mycrates::cli::fetch::run(&args.args),
        cli::Command::Publish => mycrates::cli::publish::run(&args.args),
        cli::Command::ServePublish => mycrates::cli::serve_publish::run(&args.args),
        cli::Command::Verify => mycrates::cli::verify::run(&args.args),
        cli::Command::Doctor => mycrates::cli::doctor::run(&args.args),
        cli::Command::Validate => mycrates::cli::validate::run(&args.args),
    };

    if let Err(e) = res {
        eprintln!("{e}");
        process::exit(e.exit_code());
    }
}
