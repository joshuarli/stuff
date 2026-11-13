#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::cache::{CacheObject, MirrorCache, ObjectRole};
use crate::mirror::inventory::Inventory;
use crate::mirror::json::json_escape;
use crate::mirror::lock::MirrorLock;
use crate::mirror::ownership::{decide, read_ownership, AliasAction};
use crate::mirror::MirrorOptions;
use crate::state::State;
use rustc_hash::FxHashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use stuff_oci::{Digest, RepositoryName, Tag};

#[derive(Debug)]
struct AliasReport {
    alias: String,
    action: AliasAction,
    current: Option<Digest>,
    desired: Digest,
}

#[derive(Debug)]
struct ObjectReport {
    graph: &'static str,
    present: Option<u64>,
    missing: Option<u64>,
    known_bytes: Option<u64>,
    transfer_bytes: Option<u64>,
    total_objects: Option<u64>,
}

#[derive(Debug)]
struct EntryReport {
    id: String,
    destination: String,
    root: Digest,
    aliases: Vec<AliasReport>,
    objects: ObjectReport,
}

pub fn run(options: &MirrorOptions) -> Result<()> {
    let inventory = Inventory::load(Path::new(&options.config))?;
    let lock_path = Path::new(&options.lock);
    let lock = MirrorLock::load(lock_path).map_err(|error| {
        Error::Input(format!(
            "plan requires a valid lock at {}: {error}; run `myoci mirror resolve`",
            lock_path.display()
        ))
    })?;
    lock.validate_for_inventory(&inventory)?;
    let state_dir = PathBuf::from(options.state_dir.as_deref().expect("validated"));
    let state = State::new(state_dir.clone());
    let state_present = state_dir.is_dir();
    let _lock = if state_present {
        Some(state.lock()?)
    } else {
        None
    };
    if state_present {
        let pending = state_dir.join(".mirror-transaction");
        if pending.exists() {
            return Err(Error::Conflict(format!(
                "state at {} has an unfinished mirror transaction at {}; run `myoci mirror sync --locked` to recover it before planning",
                state_dir.display(),
                pending.display()
            )));
        }
        if !state_dir.join("state.json").is_file() {
            return Err(Error::Integrity(format!(
                "{} exists but has no state.json; the state is incomplete",
                state_dir.display()
            )));
        }
    }
    let ownership = if state_present {
        read_ownership(&state_dir)?
    } else {
        FxHashMap::default()
    };
    let blobs = if state_present {
        state.read_blobs()?
    } else {
        rustc_hash::FxHashSet::default()
    };
    let cache = match MirrorCache::load_if_fresh(lock_path) {
        Ok(cache) => cache,
        Err(error) => {
            eprintln!("myoci: warning: {error}");
            None
        }
    };

    let mut entries = Vec::with_capacity(lock.entries.len());
    let mut conflicts = Vec::new();
    let mut totals = Totals::new();
    for entry in &lock.entries {
        let desired = entry.root_digest.clone();
        let destination_name = RepositoryName::parse(&entry.destination).map_err(|error| {
            Error::Integrity(format!("lock entry {:?} destination: {error}", entry.id))
        })?;
        let view = if state_present {
            state.read_repository(&destination_name)?
        } else {
            None
        };
        let mut aliases = Vec::with_capacity(entry.aliases.len());
        for alias in &entry.aliases {
            let tag = Tag::parse(alias).expect("validated at lock parse");
            let current = view
                .as_ref()
                .and_then(|view| view.tags.get(&tag).map(|descriptor| descriptor.digest.clone()));
            let key = (entry.destination.clone(), alias.clone());
            let decision = decide(current.as_ref(), &desired, ownership.get(&key));
            if decision.action.is_conflict() {
                conflicts.push((entry.id.clone(), alias.clone(), decision.action));
            }
            aliases.push(AliasReport {
                alias: alias.clone(),
                action: decision.action,
                current: decision.current,
                desired: desired.clone(),
            });
        }
        let objects = object_report(
            &state,
            state_present,
            &blobs,
            cache.as_ref(),
            &entry.id,
            &mut totals,
        );
        entries.push(EntryReport {
            id: entry.id.clone(),
            destination: entry.destination.clone(),
            root: desired,
            aliases,
            objects,
        });
    }

    if options.json {
        println!("{}", render_json(state_present, &entries, &conflicts, &totals));
    } else {
        print!("{}", render_text(&state_dir, state_present, &entries, &conflicts, &totals));
    }
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(Error::Conflict(format!(
            "{} alias conflict(s); no state was changed",
            conflicts.len()
        )))
    }
}

struct Totals {
    graph_known: bool,
    present_known: u64,
    missing_known: u64,
    transfer_known: u64,
    counted: rustc_hash::FxHashSet<Digest>,
}

impl Totals {
    fn new() -> Self {
        Self {
            graph_known: true,
            present_known: 0,
            missing_known: 0,
            transfer_known: 0,
            counted: rustc_hash::FxHashSet::default(),
        }
    }
}

fn object_report(
    state: &State,
    state_present: bool,
    blobs: &rustc_hash::FxHashSet<Digest>,
    cache: Option<&MirrorCache>,
    entry_id: &str,
    totals: &mut Totals,
) -> ObjectReport {
    let Some(cache) = cache else {
        totals.graph_known = false;
        return ObjectReport {
            graph: "unknown",
            present: None,
            missing: None,
            known_bytes: None,
            transfer_bytes: None,
            total_objects: None,
        };
    };
    let Some(objects) = cache.entry_objects(entry_id).filter(|objects| !objects.is_empty()) else {
        totals.graph_known = false;
        return ObjectReport {
            graph: "unknown",
            present: None,
            missing: None,
            known_bytes: None,
            transfer_bytes: None,
            total_objects: None,
        };
    };
    let objects: Vec<CacheObject> = objects.to_vec();
    let mut present = 0u64;
    let mut missing = 0u64;
    let mut known_bytes = 0u64;
    let mut transfer_bytes = 0u64;
    let mut seen = rustc_hash::FxHashSet::default();
    for object in &objects {
        if !seen.insert(object.digest.clone()) {
            continue;
        }
        known_bytes += object.size;
        let object_present = match object.role {
            ObjectRole::Manifest => state_present && state.manifest_path(&object.digest).is_file(),
            ObjectRole::Config | ObjectRole::Layer => state_present && blobs.contains(&object.digest),
        };
        if object_present {
            present += 1;
        } else {
            missing += 1;
            transfer_bytes += object.size;
        }
        if totals.counted.insert(object.digest.clone()) {
            totals.present_known += u64::from(object_present);
            totals.missing_known += u64::from(!object_present);
            if !object_present {
                totals.transfer_known += object.size;
            }
        }
    }
    ObjectReport {
        graph: "known",
        present: Some(present),
        missing: Some(missing),
        known_bytes: Some(known_bytes),
        transfer_bytes: Some(transfer_bytes),
        total_objects: Some(seen.len() as u64),
    }
}

fn render_text(
    state_dir: &Path,
    state_present: bool,
    entries: &[EntryReport],
    conflicts: &[(String, String, AliasAction)],
    totals: &Totals,
) -> String {
    let mut out = String::new();
    if state_present {
        out.push_str(&format!("state: present ({})\n", state_dir.display()));
    } else {
        out.push_str(&format!(
            "state: absent ({}); every object is missing\n",
            state_dir.display()
        ));
    }
    out.push_str("remote: not-checked\n");
    for entry in entries {
        out.push_str(&format!(
            "entry {} (destination {}):\n  root {}\n",
            entry.id, entry.destination, entry.root
        ));
        for alias in &entry.aliases {
            let current = alias
                .current
                .as_ref()
                .map(|digest| digest.to_string())
                .unwrap_or_else(|| "absent".to_string());
            out.push_str(&format!(
                "  alias {}: {} {} (currently {})\n",
                alias.alias,
                alias.action.as_str(),
                alias.desired,
                current
            ));
        }
        match entry.objects.graph {
            "known" => out.push_str(&format!(
                "  objects: {} present, {} missing of {}; transfer {} bytes; graph known\n",
                entry.objects.present.unwrap(),
                entry.objects.missing.unwrap(),
                entry.objects.total_objects.unwrap(),
                entry.objects.transfer_bytes.unwrap()
            )),
            _ => out.push_str(
                "  objects: graph unknown (rerun `myoci mirror resolve`); transfer bytes unknown\n",
            ),
        }
    }
    if totals.graph_known {
        out.push_str(&format!(
            "totals: {} present, {} missing; transfer {} bytes\n",
            totals.present_known, totals.missing_known, totals.transfer_known
        ));
    } else {
        out.push_str("totals: graph unknown; transfer bytes unknown\n");
    }
    if conflicts.is_empty() {
        out.push_str("plan: clean\n");
    } else {
        out.push_str(&format!("plan: {} conflict(s)\n", conflicts.len()));
        for (id, alias, action) in conflicts {
            out.push_str(&format!("  conflict {id} alias {alias}: {}\n", action.as_str()));
        }
    }
    out
}

fn render_json(
    state_present: bool,
    entries: &[EntryReport],
    conflicts: &[(String, String, AliasAction)],
    totals: &Totals,
) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"schema_version\":1,\"state\":{},\"remote\":\"not-checked\",\"entries\":[",
        json_escape(if state_present { "present" } else { "absent" })
    );
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{},\"destination\":{},\"root\":{},\"aliases\":[",
            json_escape(&entry.id),
            json_escape(&entry.destination),
            json_escape(entry.root.as_str())
        ));
        for (alias_index, alias) in entry.aliases.iter().enumerate() {
            if alias_index > 0 {
                out.push(',');
            }
            let current = alias
                .current
                .as_ref()
                .map(|digest| json_escape(digest.as_str()))
                .unwrap_or_else(|| "null".to_string());
            out.push_str(&format!(
                "{{\"alias\":{},\"action\":{},\"current\":{},\"desired\":{}}}",
                json_escape(&alias.alias),
                json_escape(alias.action.as_str()),
                current,
                json_escape(alias.desired.as_str())
            ));
        }
        out.push_str("],\"objects\":");
        match entry.objects.graph {
            "known" => out.push_str(&format!(
                "{{\"graph\":\"known\",\"present\":{},\"missing\":{},\"known_bytes\":{},\"transfer_bytes\":{},\"total_objects\":{}}}",
                entry.objects.present.unwrap(),
                entry.objects.missing.unwrap(),
                entry.objects.known_bytes.unwrap(),
                entry.objects.transfer_bytes.unwrap(),
                entry.objects.total_objects.unwrap()
            )),
            _ => out.push_str("{\"graph\":\"unknown\",\"present\":null,\"missing\":null,\"known_bytes\":null,\"transfer_bytes\":null,\"total_objects\":null}"),
        }
        out.push('}');
    }
    out.push_str("],\"totals\":");
    if totals.graph_known {
        out.push_str(&format!(
            "{{\"present\":{},\"missing\":{},\"transfer_bytes\":{}}}",
            totals.present_known, totals.missing_known, totals.transfer_known
        ));
    } else {
        out.push_str("{\"present\":null,\"missing\":null,\"transfer_bytes\":null}");
    }
    out.push_str(",\"conflicts\":[");
    for (index, (id, alias, action)) in conflicts.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{},\"alias\":{},\"action\":{}}}",
            json_escape(id),
            json_escape(alias),
            json_escape(action.as_str())
        ));
    }
    out.push_str("],\"result\":");
    out.push_str(if conflicts.is_empty() { "\"clean\"" } else { "\"conflicts\"" });
    out.push_str("}\n");
    out
}
