#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::json::json_escape;
use crate::mirror::acquire::{acquire, Acquisition};
use crate::mirror::http::RegistryHttp;
use crate::mirror::inventory::Inventory;
use crate::mirror::lock::MirrorLock;
use crate::mirror::MirrorOptions;
use crate::state::State;
use std::fmt::Write as _;
use std::path::Path;

pub fn run(options: &MirrorOptions) -> Result<()> {
    let inventory = Inventory::load(Path::new(&options.config))?;
    let lock_path = Path::new(&options.lock);
    let lock = MirrorLock::load(lock_path).map_err(|error| {
        Error::Input(format!(
            "sync requires a valid lock at {}: {error}; run `myoci mirror resolve`",
            lock_path.display()
        ))
    })?;
    lock.validate_for_inventory(&inventory)?;
    let state_dir = crate::mirror::state_dir_path(options);
    let state = State::new(state_dir);
    let http = RegistryHttp::new();
    let acquisition = acquire(&state, &inventory, &lock, &http, options.offline)?;
    if options.json {
        println!("{}", render_json(options.offline, &acquisition));
    } else {
        print!("{}", render_text(options.offline, &acquisition));
    }
    Ok(())
}

fn render_text(offline: bool, acquisition: &Acquisition) -> String {
    let mut out = String::new();
    for entry in &acquisition.entries {
        out.push_str(&format!(
            "entry {}: {} manifests, {} blobs acquired ({} bytes), {} objects\n",
            entry.id, entry.manifests, entry.blobs_fetched, entry.bytes_fetched, entry.objects
        ));
    }
    out.push_str(&format!(
        "sync{}: {} blobs acquired ({} bytes)\n",
        if offline { " (offline)" } else { "" },
        acquisition.blobs_fetched,
        acquisition.bytes_fetched
    ));
    out
}

fn render_json(offline: bool, acquisition: &Acquisition) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"schema_version\":1,\"offline\":{},\"entries\":[",
        offline
    );
    for (index, entry) in acquisition.entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{},\"manifests\":{},\"blobs_fetched\":{},\"bytes_fetched\":{},\"objects\":{}}}",
            json_escape(&entry.id),
            entry.manifests,
            entry.blobs_fetched,
            entry.bytes_fetched,
            entry.objects
        ));
    }
    out.push_str(&format!(
        "],\"totals\":{{\"blobs_fetched\":{},\"bytes_fetched\":{}}}}}\n",
        acquisition.blobs_fetched, acquisition.bytes_fetched
    ));
    out
}
