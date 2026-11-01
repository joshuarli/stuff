#![forbid(unsafe_code)]

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|arg| arg == "-h" || arg == "--help") || argv.is_empty() {
        myoci::cli::print_help();
        return;
    }
    if let Err(error) = myoci::cli::run(&argv) {
        eprintln!("myoci: {error}");
        std::process::exit(error.exit_code());
    }
}
