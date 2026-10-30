#![forbid(unsafe_code)]

use mypypi::cli;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        cli::print_help();
        return;
    }
    let parsed = match cli::parse_args(&argv) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(e.exit_code());
        }
    };
    let _ = parsed;
    eprintln!("mypypi Rust port: command implementation follows in later slices");
    std::process::exit(1);
}
