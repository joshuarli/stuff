#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::inventory::{
    integer_field, read_capped, string_array, string_field, PlatformRequirement, ReferrersMode,
    POLICY_VERSION, SCHEMA_VERSION,
};
use crate::mirror::reference::RegistryReference;
use stuff_oci::{Digest, RepositoryName, Tag};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedReferrer {
    pub subject: Digest,
    pub media_type: String,
    pub digest: Digest,
    pub size: u64,
    pub artifact_type: Option<String>,
}

impl LockedReferrer {
    fn render_key(&self) -> (&str, &str) {
        (self.subject.as_str(), self.digest.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedEntry {
    pub id: String,
    pub source: String,
    pub source_repository: String,
    pub destination: String,
    pub aliases: Vec<String>,
    pub platforms: Vec<String>,
    pub referrers: ReferrersMode,
    pub root_media_type: String,
    pub root_digest: Digest,
    pub root_size: u64,
    pub resolved_platforms: Vec<String>,
    pub additional_referrers: Vec<LockedReferrer>,
}

impl LockedEntry {
    pub fn destination_repository(&self) -> Result<RepositoryName> {
        let parsed = RegistryReference::parse(&self.destination, "lock destination")?;
        if parsed.reference.is_some() {
            return Err(Error::Integrity(format!(
                "lock entry {:?} destination {:?} carries a reference",
                self.id, self.destination
            )));
        }
        Ok(parsed.repository_name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorLock {
    pub inventory_fingerprint: Digest,
    pub entries: Vec<LockedEntry>,
}

impl MirrorLock {
    pub fn load(path: &Path) -> Result<Self> {
        let text = read_capped(path, "lock")?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let document: toml_edit::DocumentMut = text.parse().map_err(|error| {
            Error::Input(format!("lock is not valid TOML: {error}"))
        })?;
        let root = document.as_table();
        for (key, _) in root.iter() {
            match key {
                "schema_version" | "policy_version" | "inventory_fingerprint" | "entries" => {}
                other => return Err(Error::Input(format!("lock: unknown field {other:?}"))),
            }
        }
        let schema_version = integer_field(root, "schema_version", "lock")?;
        let policy_version = integer_field(root, "policy_version", "lock")?;
        if schema_version != u64::from(SCHEMA_VERSION) {
            return Err(Error::Input(format!(
                "lock: unsupported schema_version {schema_version}; expected {SCHEMA_VERSION}"
            )));
        }
        if policy_version != u64::from(POLICY_VERSION) {
            return Err(Error::Input(format!(
                "lock: unsupported policy_version {policy_version}; expected {POLICY_VERSION}"
            )));
        }
        let fingerprint_text = string_field(root, "inventory_fingerprint", "lock")?;
        let inventory_fingerprint = Digest::parse(&fingerprint_text).map_err(|error| {
            Error::Input(format!("lock: inventory_fingerprint: {error}"))
        })?;
        let entries = match root.get("entries") {
            None => Vec::new(),
            Some(item) => {
                let array = item.as_array_of_tables().ok_or_else(|| {
                    Error::Input("lock: entries must be an array of tables".to_string())
                })?;
                let mut entries = Vec::with_capacity(array.len());
                for (index, table) in array.iter().enumerate() {
                    entries.push(parse_locked_entry(table, index)?);
                }
                entries
            }
        };
        validate_entries(&entries)?;
        Ok(Self {
            inventory_fingerprint,
            entries,
        })
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "schema_version = {SCHEMA_VERSION}\npolicy_version = {POLICY_VERSION}\ninventory_fingerprint = {}\n",
            toml_string(self.inventory_fingerprint.as_str())
        ));
        let mut entries: Vec<&LockedEntry> = self.entries.iter().collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        for entry in entries {
            out.push_str("\n[[entries]]\n");
            out.push_str(&format!("id = {}\n", toml_string(&entry.id)));
            out.push_str(&format!("source = {}\n", toml_string(&entry.source)));
            out.push_str(&format!("source_repository = {}\n", toml_string(&entry.source_repository)));
            out.push_str(&format!("destination = {}\n", toml_string(&entry.destination)));
            out.push_str(&format!("aliases = {}\n", toml_string_array(&entry.aliases)));
            out.push_str(&format!("platforms = {}\n", toml_string_array(&entry.platforms)));
            out.push_str(&format!("referrers = {}\n", toml_string(entry.referrers.as_str())));
            out.push_str(&format!("root_media_type = {}\n", toml_string(&entry.root_media_type)));
            out.push_str(&format!("root_digest = {}\n", toml_string(entry.root_digest.as_str())));
            out.push_str(&format!("root_size = {}\n", entry.root_size));
            out.push_str(&format!(
                "resolved_platforms = {}\n",
                toml_string_array(&entry.resolved_platforms)
            ));
            let mut referrers: Vec<&LockedReferrer> = entry.additional_referrers.iter().collect();
            referrers.sort_by(|a, b| a.render_key().cmp(&b.render_key()));
            for referrer in referrers {
                out.push_str("\n[[entries.referrer]]\n");
                out.push_str(&format!("subject = {}\n", toml_string(referrer.subject.as_str())));
                out.push_str(&format!("media_type = {}\n", toml_string(&referrer.media_type)));
                out.push_str(&format!("digest = {}\n", toml_string(referrer.digest.as_str())));
                out.push_str(&format!("size = {}\n", referrer.size));
                if let Some(artifact_type) = &referrer.artifact_type {
                    out.push_str(&format!("artifact_type = {}\n", toml_string(artifact_type)));
                }
            }
        }
        out
    }

    pub fn entry(&self, id: &str) -> Option<&LockedEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn validate_for_inventory(&self, inventory: &crate::mirror::inventory::Inventory) -> Result<()> {
        let fingerprint = inventory.fingerprint();
        if self.inventory_fingerprint != fingerprint {
            return Err(Error::Input(format!(
                "lock inventory_fingerprint {} does not match the inventory fingerprint {}; run `myoci mirror resolve`",
                self.inventory_fingerprint, fingerprint
            )));
        }
        for entry in &self.entries {
            if inventory.entry(&entry.id).is_none() {
                return Err(Error::Input(format!(
                    "lock entry {:?} is not in the inventory; run `myoci mirror resolve`",
                    entry.id
                )));
            }
        }
        for entry in &inventory.entries {
            if self.entry(&entry.id).is_none() {
                return Err(Error::Input(format!(
                    "lock is missing entry {:?}; run `myoci mirror resolve`",
                    entry.id
                )));
            }
        }
        Ok(())
    }
}

fn parse_locked_entry(table: &toml_edit::Table, index: usize) -> Result<LockedEntry> {
    let context = format!("lock entries[{index}]");
    for (key, _) in table.iter() {
        match key {
            "id" | "source" | "source_repository" | "destination" | "aliases" | "platforms"
            | "referrers" | "root_media_type" | "root_digest" | "root_size" | "resolved_platforms"
            | "referrer" => {}
            other => return Err(Error::Input(format!("{context}: unknown field {other:?}"))),
        }
    }
    let id = string_field(table, "id", &context)?;
    if id.is_empty() || id.len() > 64 {
        return Err(Error::Input(format!("{context}: entry id is empty or too long")));
    }
    let source = string_field(table, "source", &context)?;
    let declared = RegistryReference::parse(&source, &format!("{context} source"))?;
    if declared.reference.is_none() {
        return Err(Error::Input(format!("{context} source {source:?} must name a tag or digest")));
    }
    let source_repository = string_field(table, "source_repository", &context)?;
    if source_repository != declared.repository_id() {
        return Err(Error::Input(format!(
            "{context} source_repository {source_repository:?} does not match the source"
        )));
    }
    let destination = string_field(table, "destination", &context)?;
    let parsed_destination = RegistryReference::parse(&destination, &format!("{context} destination"))?;
    if parsed_destination.reference.is_some() {
        return Err(Error::Input(format!(
            "{context} destination {destination:?} must not carry a reference"
        )));
    }
    if destination != parsed_destination.repository_id() {
        return Err(Error::Input(format!(
            "{context} destination {destination:?} is not normalized"
        )));
    }
    let aliases = string_array(table, "aliases", &context)?;
    for alias in &aliases {
        Tag::parse(alias).map_err(|error| Error::Input(format!("{context} aliases: {error}")))?;
    }
    require_sorted_unique(&aliases, "aliases", &context)?;
    let platforms = string_array(table, "platforms", &context)?;
    for platform in &platforms {
        PlatformRequirement::parse(platform, &format!("{context} platforms"))?;
    }
    require_sorted_unique(&platforms, "platforms", &context)?;
    let referrers = ReferrersMode::parse(&string_field(table, "referrers", &context)?, &format!("{context} referrers"))?;
    let root_media_type = string_field(table, "root_media_type", &context)?;
    if root_media_type.is_empty() {
        return Err(Error::Input(format!("{context}: root_media_type must not be empty")));
    }
    let root_digest = Digest::parse(&string_field(table, "root_digest", &context)?)
        .map_err(|error| Error::Input(format!("{context} root_digest: {error}")))?;
    let root_size = integer_field(table, "root_size", &context)?;
    let resolved_platforms = string_array(table, "resolved_platforms", &context)?;
    for platform in &resolved_platforms {
        PlatformRequirement::parse(platform, &format!("{context} resolved_platforms"))?;
    }
    require_sorted_unique(&resolved_platforms, "resolved_platforms", &context)?;
    let additional_referrers = parse_referrers(table, &context)?;
    Ok(LockedEntry {
        id,
        source,
        source_repository,
        destination,
        aliases,
        platforms,
        referrers,
        root_media_type,
        root_digest,
        root_size,
        resolved_platforms,
        additional_referrers,
    })
}

fn parse_referrers(table: &toml_edit::Table, context: &str) -> Result<Vec<LockedReferrer>> {
    let Some(item) = table.get("referrer") else {
        return Ok(Vec::new());
    };
    let array = item.as_array_of_tables().ok_or_else(|| {
        Error::Input(format!("{context}: referrer must be an array of tables"))
    })?;
    let mut out = Vec::with_capacity(array.len());
    for (index, referrer) in array.iter().enumerate() {
        let rctx = format!("{context} referrer[{index}]");
        for (key, _) in referrer.iter() {
            match key {
                "subject" | "media_type" | "digest" | "size" | "artifact_type" => {}
                other => return Err(Error::Input(format!("{rctx}: unknown field {other:?}"))),
            }
        }
        let subject = Digest::parse(&string_field(referrer, "subject", &rctx)?)
            .map_err(|error| Error::Input(format!("{rctx} subject: {error}")))?;
        let media_type = string_field(referrer, "media_type", &rctx)?;
        if !stuff_oci::is_manifest_or_index(&media_type) {
            return Err(Error::Input(format!(
                "{rctx}: referrer {media_type:?} is not a supported manifest or index media type"
            )));
        }
        let digest = Digest::parse(&string_field(referrer, "digest", &rctx)?)
            .map_err(|error| Error::Input(format!("{rctx} digest: {error}")))?;
        let size = integer_field(referrer, "size", &rctx)?;
        let artifact_type = match referrer.get("artifact_type") {
            Some(value) => Some(
                value.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::Input(format!("{rctx}: artifact_type must be a string")))?,
            ),
            None => None,
        };
        out.push(LockedReferrer {
            subject,
            media_type,
            digest,
            size,
            artifact_type,
        });
    }
    let mut seen = rustc_hash::FxHashSet::default();
    for referrer in &out {
        if !seen.insert((referrer.subject.as_str(), referrer.digest.as_str())) {
            return Err(Error::Input(format!(
                "{context}: duplicate referrer {} for subject {}",
                referrer.digest, referrer.subject
            )));
        }
    }
    let mut keys: Vec<(&str, &str)> = out.iter().map(LockedReferrer::render_key).collect();
    let sorted = keys.clone();
    keys.sort();
    if keys != sorted {
        return Err(Error::Input(format!(
            "{context}: referrers are not in canonical order"
        )));
    }
    Ok(out)
}

fn require_sorted_unique(values: &[String], field: &str, context: &str) -> Result<()> {
    for pair in values.windows(2) {
        if pair[0] == pair[1] {
            return Err(Error::Input(format!("{context}: duplicate {field} {:?}", pair[0])));
        }
        if pair[0] > pair[1] {
            return Err(Error::Input(format!("{context}: {field} are not in canonical order")));
        }
    }
    Ok(())
}

fn validate_entries(entries: &[LockedEntry]) -> Result<()> {
    let mut ids = rustc_hash::FxHashSet::default();
    for entry in entries {
        if !ids.insert(entry.id.as_str()) {
            return Err(Error::Input(format!("lock: duplicate entry id {:?}", entry.id)));
        }
    }
    let mut keys: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    let sorted = keys.clone();
    keys.sort();
    if keys != sorted {
        return Err(Error::Input("lock: entries are not in canonical order".to_string()));
    }
    let mut tag_owners: rustc_hash::FxHashMap<(String, String), String> = rustc_hash::FxHashMap::default();
    for entry in entries {
        let destination = entry.destination.clone();
        for alias in &entry.aliases {
            if let Some(owner) = tag_owners.insert((destination.clone(), alias.clone()), entry.id.clone()) {
                return Err(Error::Input(format!(
                    "lock: alias {alias:?} at destination {destination} is assigned to both {owner:?} and {:?}",
                    entry.id
                )));
            }
        }
    }
    Ok(())
}

pub fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ch if (ch as u32) < 0x20 || ch as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04X}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

pub fn toml_string_array(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|value| toml_string(value)).collect();
    format!("[{}]", items.join(", "))
}
