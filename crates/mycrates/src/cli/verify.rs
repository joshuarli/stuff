use crate::acquire::verify;
use crate::cli::{reject_unknown, take_value};
use crate::error::Result;
use std::env;
use std::path::PathBuf;

const USAGE: &str = "mycrates verify [--state-dir DIR]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            other => {
                return reject_unknown(other, USAGE);
            }
        }
    }

    verify(&state_dir)?;
    println!("verification passed: all blobs and records intact");
    Ok(())
}
