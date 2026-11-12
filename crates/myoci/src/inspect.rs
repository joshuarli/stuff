#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::State;
use rustc_hash::FxHashSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    pub tags: Vec<String>,
    pub manifests: Vec<String>,
    pub blobs: Vec<String>,
    pub referrers: Vec<String>,
}

impl Inspection {
    pub fn render_json(&self) -> String {
        let mut out = String::from("{\n");
        out.push_str(&format!("  \"tags\": {},\n", render_array(&self.tags)));
        out.push_str(&format!("  \"manifests\": {},\n", render_array(&self.manifests)));
        out.push_str(&format!("  \"blobs\": {},\n", render_array(&self.blobs)));
        out.push_str(&format!("  \"referrers\": {}\n", render_array(&self.referrers)));
        out.push_str("}\n");
        out
    }
}

fn render_array(values: &[String]) -> String {
    let values: Vec<String> = values.iter().map(|value| quoted(value)).collect();
    format!("[{}]", values.join(", "))
}

fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn inspect(state: &State) -> Result<Inspection> {
    let mut tags = FxHashSet::default();
    let mut manifests = FxHashSet::default();
    let mut referrers = FxHashSet::default();
    for (repository, path) in state.repository_files()? {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        let view = stuff_oci::parse_repository_json(&repository, &text)
            .map_err(|error| Error::Integrity(error.to_string()))?;
        for tag in view.tags.keys() {
            tags.insert(tag.as_str().to_string());
        }
        for digest in view.manifests.keys() {
            manifests.insert(digest.as_str().to_string());
        }
        collect_referrer_subjects(&state.referrers_dir(&repository), &mut referrers)?;
    }
    let mut tags: Vec<String> = tags.into_iter().collect();
    tags.sort_unstable();
    let mut manifests: Vec<String> = manifests.into_iter().collect();
    manifests.sort_unstable();
    let mut blobs: Vec<String> = state
        .read_blobs()?
        .iter()
        .map(|digest| digest.as_str().to_string())
        .collect();
    blobs.sort_unstable();
    let mut referrers: Vec<String> = referrers.into_iter().collect();
    referrers.sort_unstable();
    Ok(Inspection { tags, manifests, blobs, referrers })
}

fn collect_referrer_subjects(dir: &Path, out: &mut FxHashSet<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::Operation(format!("could not read {}: {error}", dir.display())));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", dir.display())))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(hex) = name.strip_suffix(".json") else { continue };
        if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            out.insert(format!("sha256:{hex}"));
        }
    }
    Ok(())
}
