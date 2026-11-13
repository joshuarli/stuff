#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::cache::{EntryCache, MirrorCache};
use crate::mirror::graph::resolve_entry;
use crate::mirror::http::{registry_base, RegistryHttp};
use crate::mirror::inventory::Inventory;
use crate::mirror::lock::{LockedEntry, MirrorLock};
use crate::mirror::MirrorOptions;
use std::path::Path;

pub fn run(options: &MirrorOptions) -> Result<()> {
    let inventory = Inventory::load(Path::new(&options.config))?;
    let lock_path = Path::new(&options.lock);
    let old_cache = match MirrorCache::load_if_fresh(lock_path) {
        Ok(cache) => cache,
        Err(error) => {
            eprintln!("myoci: warning: discarding the previous lock cache: {error}");
            None
        }
    };
    let selected = dedupe(&options.entries);
    let existing = load_existing(lock_path, &inventory, &selected)?;
    let mut entries = Vec::with_capacity(inventory.entries.len());
    let http = RegistryHttp::new();
    let mut preserved = 0usize;
    let mut resolved_objects: rustc_hash::FxHashMap<String, Vec<crate::mirror::cache::CacheObject>> = rustc_hash::FxHashMap::default();
    for entry in &inventory.entries {
        let resolved = if !selected.is_empty() && !selected.contains(&entry.id) {
            preserved += 1;
            existing
                .as_ref()
                .and_then(|lock| lock.entry(&entry.id))
                .expect("entry membership checked")
                .clone()
        } else {
            let (locked, objects) = resolve_one(&http, entry)?;
            resolved_objects.insert(entry.id.clone(), objects);
            locked
        };
        entries.push(resolved);
    }
    let lock = MirrorLock {
        inventory_fingerprint: inventory.fingerprint(),
        entries,
    };
    let rendered = lock.render();
    stuff_store::write_file_durable(lock_path, rendered.as_bytes())?;
    let cache = build_cache(&old_cache, rendered.as_bytes(), &inventory, &resolved_objects)?;
    if let Err(error) = stuff_store::write_file_durable(&MirrorCache::path_for(lock_path), cache.render().as_bytes()) {
        eprintln!(
            "myoci: warning: could not write the lock cache at {}: {error}; plan estimates will report unknown",
            MirrorCache::path_for(lock_path).display()
        );
    }
    for entry in &lock.entries {
        println!(
            "resolved {} root {} ({} additional referrers)",
            entry.id,
            entry.root_digest,
            entry.additional_referrers.len()
        );
    }
    println!("wrote {} ({} entries, {preserved} preserved)", lock_path.display(), lock.entries.len());
    Ok(())
}

fn build_cache(
    old_cache: &Option<MirrorCache>,
    rendered_lock: &[u8],
    inventory: &Inventory,
    resolved_objects: &rustc_hash::FxHashMap<String, Vec<crate::mirror::cache::CacheObject>>,
) -> Result<MirrorCache> {
    let mut entries = Vec::with_capacity(inventory.entries.len());
    for entry in &inventory.entries {
        let objects = match resolved_objects.get(&entry.id) {
            Some(objects) if !objects.is_empty() => objects.clone(),
            _ => match old_cache
                .as_ref()
                .and_then(|cache| cache.entry_objects(&entry.id))
            {
                Some(objects) if !objects.is_empty() => objects.to_vec(),
                _ => continue,
            },
        };
        entries.push(EntryCache {
            id: entry.id.clone(),
            objects,
        });
    }
    Ok(MirrorCache {
        lock_digest: stuff_oci::Digest::from_bytes(rendered_lock),
        entries,
    })
}

fn dedupe(entries: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        if !out.contains(entry) {
            out.push(entry.clone());
        }
    }
    out
}

fn load_existing(
    lock_path: &Path,
    inventory: &Inventory,
    selected: &[String],
) -> Result<Option<MirrorLock>> {
    if selected.is_empty() {
        return Ok(None);
    }
    let lock = MirrorLock::load(lock_path).map_err(|error| {
        Error::Input(format!(
            "resolve --entry requires an existing valid lock at {}: {error}",
            lock_path.display()
        ))
    })?;
    let lock_ids: rustc_hash::FxHashSet<&str> = lock.entries.iter().map(|entry| entry.id.as_str()).collect();
    let inventory_ids: rustc_hash::FxHashSet<&str> = inventory.entries.iter().map(|entry| entry.id.as_str()).collect();
    let missing_from_lock: Vec<&str> = inventory_ids.difference(&lock_ids).copied().collect();
    let missing_from_inventory: Vec<&str> = lock_ids.difference(&inventory_ids).copied().collect();
    if !missing_from_lock.is_empty() || !missing_from_inventory.is_empty() {
        let mut detail = String::new();
        if !missing_from_lock.is_empty() {
            detail.push_str(&format!(" missing from the lock: {};", missing_from_lock.join(", ")));
        }
        if !missing_from_inventory.is_empty() {
            detail.push_str(&format!(" missing from the inventory: {};", missing_from_inventory.join(", ")));
        }
        return Err(Error::Input(format!(
            "lock and inventory disagree;{detail} rerun `myoci mirror resolve` without --entry"
        )));
    }
    for name in selected {
        if inventory.entry(name).is_none() {
            return Err(Error::Input(format!(
                "selected entry {name:?} is not in the inventory"
            )));
        }
    }
    for entry in &inventory.entries {
        if selected.contains(&entry.id) {
            continue;
        }
        let locked = lock.entry(&entry.id).expect("membership checked");
        ensure_unselected_unchanged(entry, locked)?;
    }
    Ok(Some(lock))
}

fn ensure_unselected_unchanged(
    entry: &crate::mirror::inventory::InventoryEntry,
    locked: &LockedEntry,
) -> Result<()> {
    let locked_source = crate::mirror::reference::RegistryReference::parse(&locked.source, "lock source")?;
    if locked_source.repository_id() != entry.source.repository_id()
        || locked_source.reference != entry.source.reference
    {
        return Err(Error::Input(format!(
            "unselected entry {:?} changed in the inventory; rerun `myoci mirror resolve` or select it with --entry",
            entry.id
        )));
    }
    if locked.destination != entry.destination.repository_id() {
        return Err(Error::Input(format!(
            "unselected entry {:?} changed destination; rerun `myoci mirror resolve` or select it with --entry",
            entry.id
        )));
    }
    let aliases: Vec<String> = entry.aliases.iter().map(|alias| alias.as_str().to_string()).collect();
    if locked.aliases != aliases {
        return Err(Error::Input(format!(
            "unselected entry {:?} changed aliases; rerun `myoci mirror resolve` or select it with --entry",
            entry.id
        )));
    }
    let platforms: Vec<String> = entry.platforms.iter().map(crate::mirror::PlatformRequirement::render).collect();
    if locked.platforms != platforms {
        return Err(Error::Input(format!(
            "unselected entry {:?} changed platforms; rerun `myoci mirror resolve` or select it with --entry",
            entry.id
        )));
    }
    if locked.referrers != entry.referrers {
        return Err(Error::Input(format!(
            "unselected entry {:?} changed referrers mode; rerun `myoci mirror resolve` or select it with --entry",
            entry.id
        )));
    }
    Ok(())
}

fn resolve_one(
    http: &RegistryHttp,
    entry: &crate::mirror::inventory::InventoryEntry,
) -> Result<(LockedEntry, Vec<crate::mirror::cache::CacheObject>)> {
    let base = registry_base(&entry.source.registry, &entry.source.repository);
    let outcome = resolve_entry(http, &base, &entry.source.repository, entry)?;
    if outcome.skipped_referrers > 0 {
        eprintln!(
            "myoci: entry {:?}: {} attachment(s) with unsupported media types were not recorded",
            entry.id, outcome.skipped_referrers
        );
    }
    let mut aliases: Vec<String> = entry.aliases.iter().map(|alias| alias.as_str().to_string()).collect();
    aliases.sort();
    let mut platforms: Vec<String> = entry.platforms.iter().map(crate::mirror::PlatformRequirement::render).collect();
    platforms.sort();
    Ok((
        LockedEntry {
            id: entry.id.clone(),
            source: entry.source.declared.clone(),
            source_repository: entry.source.repository_id(),
            destination: entry.destination.repository_id(),
            aliases,
            platforms,
            referrers: entry.referrers,
            root_media_type: outcome.root_media_type,
            root_digest: outcome.root_digest,
            root_size: outcome.root_size,
            resolved_platforms: outcome.resolved_platforms,
            additional_referrers: outcome.additional_referrers,
        },
        outcome.objects,
    ))
}
