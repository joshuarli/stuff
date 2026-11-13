#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::acquire::{acquire, Acquisition};
use crate::mirror::http::RegistryHttp;
use crate::mirror::inventory::Inventory;
use crate::mirror::json::json_escape;
use crate::mirror::lock::MirrorLock;
use crate::mirror::ownership::{decide, read_ownership, render_ownership, AliasAction, AliasOwnership};
use crate::mirror::transaction::{commit_locked, recover_locked, stage_file};
use crate::mirror::MirrorOptions;
use crate::state::State;
use rustc_hash::FxHashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use stuff_oci::{Digest, RepositoryName, Tag};

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
    let report = commit(&state, &lock, &acquisition)?;
    if options.json {
        println!("{}", render_json(options.offline, &acquisition, &report));
    } else {
        print!("{}", render_text(options.offline, &acquisition, &report));
    }
    Ok(())
}

pub struct AliasReport {
    pub entry: String,
    pub alias: String,
    pub action: AliasAction,
}

pub struct CommitReport {
    pub aliases: Vec<AliasReport>,
    pub recovered: bool,
}

fn commit(state: &State, lock: &MirrorLock, acquisition: &Acquisition) -> Result<CommitReport> {
    let _lock = state.lock()?;
    let recovery = recover_locked(state)?;
    let recovered = recovery != crate::mirror::transaction::Recovery::Noop;
    let ownership = read_ownership(state.root())?;

    // Pass 1: revalidate every alias decision against the current state.
    // Nothing is staged or applied until the whole batch is known-good.
    let mut planned = Vec::with_capacity(lock.entries.len());
    let mut conflicts: Vec<(String, String, AliasAction)> = Vec::new();
    for (entry, acquired) in lock.entries.iter().zip(&acquisition.entries) {
        let destination = RepositoryName::parse(&entry.destination).map_err(|error| {
            Error::Integrity(format!("lock entry {:?} destination: {error}", entry.id))
        })?;
        let view = state.read_repository(&destination)?;
        let view = view.unwrap_or_else(|| stuff_oci::RepositoryView::new(destination.clone()));
        let mut decisions = Vec::with_capacity(entry.aliases.len());
        for alias in &entry.aliases {
            let tag = Tag::parse(alias).expect("validated at lock parse");
            let current = view.tags.get(&tag).map(|descriptor| descriptor.digest.clone());
            let key = (entry.destination.clone(), alias.clone());
            let decision = decide(current.as_ref(), &entry.root_digest, ownership.get(&key));
            if decision.action.is_conflict() {
                conflicts.push((entry.id.clone(), alias.clone(), decision.action));
            }
            decisions.push((tag, decision));
        }
        planned.push((entry, acquired, destination, view, decisions));
    }
    if !conflicts.is_empty() {
        let mut message = format!("{} alias conflict(s); no aliases were applied:", conflicts.len());
        for (id, alias, action) in &conflicts {
            let _ = write!(message, " {id} {alias}={};", action.as_str());
        }
        return Err(Error::Conflict(message));
    }

    // Pass 2: stage the complete metadata snapshot.
    let mut managed: FxHashMap<(String, String), AliasOwnership> = FxHashMap::default();
    let mut aliases = Vec::new();
    for (entry, acquired, destination, view, decisions) in planned {
        let mut view = view;
        for digest in &acquired.manifests {
            let bytes = state.read_manifest(digest)?;
            let image = stuff_oci::parse_image(&bytes)
                .map_err(|error| Error::Integrity(format!("manifest {digest}: {error}")))?;
            view.insert_manifest(image.descriptor(digest.clone(), bytes.len() as u64));
        }
        let root_descriptor = view.manifests.get(&entry.root_digest).cloned().ok_or_else(|| {
            Error::Integrity(format!(
                "entry {:?}: root manifest {} is not in state after acquisition",
                entry.id, entry.root_digest
            ))
        })?;
        for (tag, decision) in &decisions {
            match decision.action {
                AliasAction::ConflictUnmanaged | AliasAction::ConflictStale => unreachable!("conflicts abort the batch"),
                AliasAction::Create | AliasAction::Move | AliasAction::Unchanged | AliasAction::Adopt => {
                    view.tag(tag.clone(), root_descriptor.clone());
                    aliases.push(AliasReport {
                        entry: entry.id.clone(),
                        alias: tag.as_str().to_string(),
                        action: decision.action,
                    });
                    managed.insert(
                        (entry.destination.clone(), tag.as_str().to_string()),
                        AliasOwnership {
                            digest: entry.root_digest.clone(),
                            entry: entry.id.clone(),
                        },
                    );
                }
            }
        }
        let repository_json = relative_repository_json(&destination);
        stage_file(state, &repository_json, stuff_oci::render_repository_json(&view).as_bytes())?;
        for (subject, descriptors) in state.compute_referrers(&view)? {
            let relative = relative_referrers(&destination, &subject);
            stage_file(state, &relative, stuff_oci::render_referrers(&descriptors).as_bytes())?;
        }
    }

    let mut blob_digests = state.read_blobs()?;
    for entry in &acquisition.entries {
        for digest in &entry.blobs {
            blob_digests.insert(digest.clone());
        }
    }
    stage_file(state, Path::new("blobs.json"), stuff_oci::render_blobs_json(&blob_digests).as_bytes())?;
    stage_file(state, Path::new("mirror.json"), render_ownership(&managed).as_bytes())?;

    commit_locked(state)?;
    aliases.sort_by(|a, b| a.alias.cmp(&b.alias).then(a.entry.cmp(&b.entry)));
    Ok(CommitReport { aliases, recovered })
}

fn relative_repository_json(destination: &RepositoryName) -> PathBuf {
    let mut path = PathBuf::from("repositories");
    for component in destination.as_str().split('/') {
        path.push(component);
    }
    path.push("repository.json");
    path
}

fn relative_referrers(destination: &RepositoryName, subject: &Digest) -> PathBuf {
    let mut path = relative_repository_json(destination);
    path.pop();
    path.push("referrers/sha256");
    path.push(format!("{}.json", subject.hex()));
    path
}

fn render_text(offline: bool, acquisition: &Acquisition, report: &CommitReport) -> String {
    let mut out = String::new();
    for entry in &acquisition.entries {
        out.push_str(&format!(
            "entry {}: {} manifests, {} blobs acquired ({} bytes), {} objects\n",
            entry.id,
            entry.manifests.len(),
            entry.blobs_fetched,
            entry.bytes_fetched,
            entry.objects
        ));
    }
    if report.recovered {
        out.push_str("recovered an interrupted mirror transaction\n");
    }
    for alias in &report.aliases {
        out.push_str(&format!(
            "entry {}: alias {} {}\n",
            alias.entry,
            alias.alias,
            alias.action.as_str()
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

fn render_json(offline: bool, acquisition: &Acquisition, report: &CommitReport) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"schema_version\":1,\"offline\":{},\"recovered\":{},\"entries\":[",
        offline, report.recovered
    );
    for (index, entry) in acquisition.entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{},\"manifests\":{},\"blobs_fetched\":{},\"bytes_fetched\":{},\"objects\":{}}}",
            json_escape(&entry.id),
            entry.manifests.len(),
            entry.blobs_fetched,
            entry.bytes_fetched,
            entry.objects
        ));
    }
    out.push_str("],\"aliases\":[");
    for (index, alias) in report.aliases.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"entry\":{},\"alias\":{},\"action\":{}}}",
            json_escape(&alias.entry),
            json_escape(&alias.alias),
            json_escape(alias.action.as_str())
        ));
    }
    out.push_str(&format!(
        "],\"totals\":{{\"blobs_fetched\":{},\"bytes_fetched\":{}}}}}\n",
        acquisition.blobs_fetched, acquisition.bytes_fetched
    ));
    out
}

