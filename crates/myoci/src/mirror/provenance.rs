#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::json::json_escape;
use crate::mirror::inventory::POLICY_VERSION;
use crate::mirror::lock::MirrorLock;
use crate::mirror::ownership::AliasOwnership;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

pub const PROVENANCE_FILE: &str = "provenance.json";
const MAX_PROVENANCE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceReferrer {
    pub subject: String,
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    pub artifact_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceEntry {
    pub id: String,
    pub source: String,
    pub source_repository: String,
    pub destination: String,
    pub aliases: Vec<String>,
    pub platforms: Vec<String>,
    pub resolved_platforms: Vec<String>,
    pub root: String,
    pub root_media_type: String,
    pub root_size: u64,
    pub referrers: Vec<ProvenanceReferrer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceOwnership {
    pub destination: String,
    pub alias: String,
    pub digest: String,
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub fingerprint: String,
    pub policy: u64,
    pub entries: Vec<ProvenanceEntry>,
    pub ownership: Vec<ProvenanceOwnership>,
}

impl Provenance {
    pub fn build(lock: &MirrorLock, managed: &FxHashMap<(String, String), AliasOwnership>) -> Result<Self> {
        let mut entries = Vec::with_capacity(lock.entries.len());
        let mut ownership = Vec::new();
        for entry in &lock.entries {
            let mut referrers = Vec::with_capacity(entry.additional_referrers.len());
            for referrer in &entry.additional_referrers {
                referrers.push(ProvenanceReferrer {
                    subject: referrer.subject.as_str().to_string(),
                    media_type: referrer.media_type.clone(),
                    digest: referrer.digest.as_str().to_string(),
                    size: referrer.size,
                    artifact_type: referrer.artifact_type.clone(),
                });
            }
            entries.push(ProvenanceEntry {
                id: entry.id.clone(),
                source: entry.source.clone(),
                source_repository: entry.source_repository.clone(),
                destination: entry.destination.clone(),
                aliases: entry.aliases.clone(),
                platforms: entry.platforms.clone(),
                resolved_platforms: entry.resolved_platforms.clone(),
                root: entry.root_digest.as_str().to_string(),
                root_media_type: entry.root_media_type.clone(),
                root_size: entry.root_size,
                referrers,
            });
            for alias in &entry.aliases {
                let record = managed.get(&(entry.destination.clone(), alias.clone())).ok_or_else(|| {
                    Error::Integrity(format!(
                        "entry {:?}: applied alias {alias:?} has no ownership record",
                        entry.id
                    ))
                })?;
                ownership.push(ProvenanceOwnership {
                    destination: entry.destination.clone(),
                    alias: alias.clone(),
                    digest: record.digest.as_str().to_string(),
                    entry: record.entry.clone(),
                });
            }
        }
        ownership.sort_by(|a, b| a.destination.cmp(&b.destination).then(a.alias.cmp(&b.alias)));
        Ok(Self {
            fingerprint: lock.inventory_fingerprint.as_str().to_string(),
            policy: u64::from(POLICY_VERSION),
            entries,
            ownership,
        })
    }

    pub fn ownership_map(&self) -> Result<FxHashMap<(String, String), AliasOwnership>> {
        let mut map = FxHashMap::default();
        for record in &self.ownership {
            let digest = stuff_oci::Digest::parse(&record.digest)
                .map_err(|error| Error::Integrity(format!("provenance ownership digest {:?}: {error}", record.digest)))?;
            map.insert(
                (record.destination.clone(), record.alias.clone()),
                AliasOwnership { digest, entry: record.entry.clone() },
            );
        }
        Ok(map)
    }

    pub fn render(&self) -> String {
        let mut out = String::from("{\"schemaVersion\":1,");
        out.push_str(&format!(
            "\"fingerprint\":{},\"policy\":{},\"entries\":[",
            json_escape(&self.fingerprint),
            self.policy
        ));
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"id\":{},\"source\":{},\"sourceRepository\":{},\"destination\":{},\"aliases\":[{}],\"platforms\":[{}],\"resolvedPlatforms\":[{}],\"root\":{},\"rootMediaType\":{},\"rootSize\":{},\"referrers\":[",
                json_escape(&entry.id),
                json_escape(&entry.source),
                json_escape(&entry.source_repository),
                json_escape(&entry.destination),
                render_string_list(&entry.aliases),
                render_string_list(&entry.platforms),
                render_string_list(&entry.resolved_platforms),
                json_escape(&entry.root),
                json_escape(&entry.root_media_type),
                entry.root_size
            ));
            for (referrer_index, referrer) in entry.referrers.iter().enumerate() {
                if referrer_index > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"subject\":{},\"mediaType\":{},\"digest\":{},\"size\":{}",
                    json_escape(&referrer.subject),
                    json_escape(&referrer.media_type),
                    json_escape(&referrer.digest),
                    referrer.size
                ));
                if let Some(artifact_type) = &referrer.artifact_type {
                    out.push_str(&format!(",\"artifactType\":{}", json_escape(artifact_type)));
                }
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("],\"ownership\":[");
        for (index, record) in self.ownership.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"destination\":{},\"alias\":{},\"digest\":{},\"entry\":{}}}",
                json_escape(&record.destination),
                json_escape(&record.alias),
                json_escape(&record.digest),
                json_escape(&record.entry)
            ));
        }
        out.push_str("]}");
        out
    }
}

fn render_string_list(values: &[String]) -> String {
    values.iter().map(|value| json_escape(value)).collect::<Vec<_>>().join(",")
}

fn provenance_path(state_root: &Path) -> PathBuf {
    state_root.join(PROVENANCE_FILE)
}

pub fn read_provenance(state_root: &Path) -> Result<Option<Provenance>> {
    let path = provenance_path(state_root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::Operation(format!("could not read {}: {error}", path.display())));
        }
    };
    if bytes.len() as u64 > MAX_PROVENANCE_BYTES {
        return Err(Error::Limit(format!("{} exceeds the size limit", path.display())));
    }
    Ok(Some(parse(&bytes)?))
}

pub fn write_provenance(state_root: &Path, provenance: &Provenance) -> Result<()> {
    let path = provenance_path(state_root);
    stuff_store::write_file_durable(&path, provenance.render().as_bytes())?;
    Ok(())
}

fn parse(bytes: &[u8]) -> Result<Provenance> {
    let value: deser_value::Value = deser_json::from_slice(bytes)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE} is not valid JSON: {error}")))?;
    let map = value
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE} must be a JSON object")))?;
    for key in map.keys() {
        match key.as_str() {
            Some("schemaVersion" | "fingerprint" | "policy" | "entries" | "ownership") => {}
            other => {
                return Err(Error::Integrity(format!(
                    "{PROVENANCE_FILE}: unknown field {:?}",
                    other.unwrap_or("<non-string>")
                )));
            }
        }
    }
    let schema_version = map
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: schemaVersion must be an integer")))?;
    if schema_version != 1 {
        return Err(Error::Integrity(format!(
            "{PROVENANCE_FILE}: unsupported schemaVersion {schema_version}"
        )));
    }
    let fingerprint = map
        .get("fingerprint")
        .and_then(|value| value.as_str())
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: fingerprint must be a string")))?;
    stuff_oci::Digest::parse(fingerprint)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: fingerprint: {error}")))?;
    let policy = map
        .get("policy")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: policy must be an integer")))?;
    if policy != u64::from(POLICY_VERSION) {
        return Err(Error::Integrity(format!(
            "{PROVENANCE_FILE}: unsupported policy {policy}; expected {POLICY_VERSION}"
        )));
    }
    let mut entries = Vec::new();
    let mut ids = std::collections::HashSet::new();
    let entries_value = map
        .get("entries")
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entries is missing")))?;
    let entries_array = entries_value
        .as_seq()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entries must be an array")))?;
    for item in entries_array.iter() {
        let entry = parse_entry(item)?;
        if !ids.insert(entry.id.clone()) {
            return Err(Error::Integrity(format!(
                "{PROVENANCE_FILE}: duplicate entry {:?}",
                entry.id
            )));
        }
        entries.push(entry);
    }
    let mut ownership = Vec::new();
    let mut aliases = std::collections::HashSet::new();
    let ownership_value = map
        .get("ownership")
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: ownership is missing")))?;
    let ownership_array = ownership_value
        .as_seq()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: ownership must be an array")))?;
    for item in ownership_array.iter() {
        let record = parse_ownership(item)?;
        if !aliases.insert((record.destination.clone(), record.alias.clone())) {
            return Err(Error::Integrity(format!(
                "{PROVENANCE_FILE}: duplicate ownership {:?} {}",
                record.destination, record.alias
            )));
        }
        ownership.push(record);
    }
    Ok(Provenance {
        fingerprint: fingerprint.to_string(),
        policy,
        entries,
        ownership,
    })
}

fn parse_entry(item: &deser_value::Value) -> Result<ProvenanceEntry> {
    let object = item
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entries must contain objects")))?;
    for key in object.keys() {
        match key.as_str() {
            Some(
                "id" | "source" | "sourceRepository" | "destination" | "aliases" | "platforms"
                | "resolvedPlatforms" | "root" | "rootMediaType" | "rootSize" | "referrers",
            ) => {}
            other => {
                return Err(Error::Integrity(format!(
                    "{PROVENANCE_FILE}: entry unknown field {:?}",
                    other.unwrap_or("<non-string>")
                )));
            }
        }
    }
    let string_field = |key: &str| -> Result<String> {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {key} must be a string")))
    };
    let id = string_field("id")?;
    let source = string_field("source")?;
    let source_repository = string_field("sourceRepository")?;
    let destination = string_field("destination")?;
    let aliases = string_list(object, "aliases")?;
    let platforms = string_list(object, "platforms")?;
    let resolved_platforms = string_list(object, "resolvedPlatforms")?;
    let root = string_field("root")?;
    stuff_oci::Digest::parse(&root)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: entry {id:?} root: {error}")))?;
    let root_media_type = string_field("rootMediaType")?;
    let root_size = object
        .get("rootSize")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {id:?} rootSize must be an integer")))?;
    let referrers_value = object
        .get("referrers")
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {id:?} referrers is missing")))?;
    let referrers_array = referrers_value
        .as_seq()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {id:?} referrers must be an array")))?;
    let mut referrers = Vec::with_capacity(referrers_array.len());
    for item in referrers_array.iter() {
        referrers.push(parse_referrer(&id, item)?);
    }
    Ok(ProvenanceEntry {
        id,
        source,
        source_repository,
        destination,
        aliases,
        platforms,
        resolved_platforms,
        root,
        root_media_type,
        root_size,
        referrers,
    })
}

fn parse_referrer(entry: &str, item: &deser_value::Value) -> Result<ProvenanceReferrer> {
    let object = item
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {entry:?} referrers must contain objects")))?;
    for key in object.keys() {
        match key.as_str() {
            Some("subject" | "mediaType" | "digest" | "size" | "artifactType") => {}
            other => {
                return Err(Error::Integrity(format!(
                    "{PROVENANCE_FILE}: entry {entry:?} referrer unknown field {:?}",
                    other.unwrap_or("<non-string>")
                )));
            }
        }
    }
    let string_field = |key: &str| -> Result<String> {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {entry:?} referrer {key} must be a string")))
    };
    let subject = string_field("subject")?;
    let digest = string_field("digest")?;
    stuff_oci::Digest::parse(&digest)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: entry {entry:?} referrer digest: {error}")))?;
    stuff_oci::Digest::parse(&subject)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: entry {entry:?} referrer subject: {error}")))?;
    let media_type = string_field("mediaType")?;
    let size = object
        .get("size")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {entry:?} referrer size must be an integer")))?;
    let artifact_type = match object.get("artifactType") {
        Some(value) => Some(
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: entry {entry:?} referrer artifactType must be a string")))?,
        ),
        None => None,
    };
    Ok(ProvenanceReferrer { subject, media_type, digest, size, artifact_type })
}

fn parse_ownership(item: &deser_value::Value) -> Result<ProvenanceOwnership> {
    let object = item
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: ownership must contain objects")))?;
    for key in object.keys() {
        match key.as_str() {
            Some("destination" | "alias" | "digest" | "entry") => {}
            other => {
                return Err(Error::Integrity(format!(
                    "{PROVENANCE_FILE}: ownership unknown field {:?}",
                    other.unwrap_or("<non-string>")
                )));
            }
        }
    }
    let string_field = |key: &str| -> Result<String> {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: ownership {key} must be a string")))
    };
    let destination = string_field("destination")?;
    stuff_oci::RepositoryName::parse(&destination)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: ownership destination {destination:?}: {error}")))?;
    let alias = string_field("alias")?;
    stuff_oci::Tag::parse(&alias)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: ownership alias {alias:?}: {error}")))?;
    let digest = string_field("digest")?;
    stuff_oci::Digest::parse(&digest)
        .map_err(|error| Error::Integrity(format!("{PROVENANCE_FILE}: ownership digest: {error}")))?;
    let entry = string_field("entry")?;
    Ok(ProvenanceOwnership { destination, alias, digest, entry })
}

fn string_list(object: &deser_value::Map, key: &str) -> Result<Vec<String>> {
    let value = object
        .get(key)
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: field {key} is missing")))?;
    let array = value
        .as_seq()
        .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: field {key} must be an array")))?;
    let mut out = Vec::with_capacity(array.len());
    for item in array.iter() {
        out.push(
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::Integrity(format!("{PROVENANCE_FILE}: field {key} must contain strings")))?,
        );
    }
    Ok(out)
}
