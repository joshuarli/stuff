use crate::adopt::adopt_site;
use crate::cli::{reject_unknown, take_value};
use crate::error::{Error, Result};
use crate::store::Store;
use std::env;
use std::path::PathBuf;

const USAGE: &str = "mycrates adopt --from-site DIR [--state-dir DIR]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut from_site: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            "--from-site" => {
                from_site = Some(PathBuf::from(take_value(args, &mut i, "--from-site")?));
                i += 1;
            }
            other => {
                return reject_unknown(other, USAGE);
            }
        }
    }

    let site_dir = from_site.ok_or_else(|| Error::Input(format!("usage: {USAGE}")))?;
    let store = Store::new(state_dir);
    let summary = adopt_site(&store, &site_dir)?;
    println!(
        "adopted {} native and {} upstream index entries ({} new blobs)",
        summary.native, summary.upstream, summary.blobs
    );
    Ok(())
}
