#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        mynpm::cli::print_help();
        return;
    }
    if let Err(e) = mynpm::cli::run(&argv) {
        eprintln!("mynpm: {e}");
        std::process::exit(e.exit_code());
    }
}
