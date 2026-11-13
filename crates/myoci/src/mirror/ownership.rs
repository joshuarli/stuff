#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::json::json_escape;
use rustc_hash::FxHashMap;
use stuff_oci::{Digest, RepositoryName, Tag};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasOwnership {
    pub digest: Digest,
    pub entry: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasDecision {
    pub action: AliasAction,
    pub current: Option<Digest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasAction {
    Create,
    Move,
    Unchanged,
    Adopt,
    ConflictUnmanaged,
    ConflictStale,
}

impl AliasAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Move => "move",
            Self::Unchanged => "unchanged",
            Self::Adopt => "adopt",
            Self::ConflictUnmanaged => "conflict-unmanaged",
            Self::ConflictStale => "conflict-stale",
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::ConflictUnmanaged | Self::ConflictStale)
    }
}

pub fn decide(
    current: Option<&Digest>,
    desired: &Digest,
    ownership: Option<&AliasOwnership>,
) -> AliasDecision {
    let Some(current) = current else {
        return AliasDecision {
            action: AliasAction::Create,
            current: None,
        };
    };
    if current == desired {
        return AliasDecision {
            action: match ownership {
                Some(_) => AliasAction::Unchanged,
                None => AliasAction::Adopt,
            },
            current: Some(current.clone()),
        };
    }
    match ownership {
        Some(record) if record.digest == *current => AliasDecision {
            action: AliasAction::Move,
            current: Some(current.clone()),
        },
        Some(_) => AliasDecision {
            action: AliasAction::ConflictStale,
            current: Some(current.clone()),
        },
        None => AliasDecision {
            action: AliasAction::ConflictUnmanaged,
            current: Some(current.clone()),
        },
    }
}

pub fn read_ownership(state_root: &Path) -> Result<FxHashMap<(String, String), AliasOwnership>> {
    let path = state_root.join("mirror.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(FxHashMap::default()),
        Err(error) => {
            return Err(Error::Operation(format!(
                "could not read {}: {error}",
                path.display()
            )))
        }
    };
    if bytes.len() as u64 > crate::mirror::inventory::MAX_CONFIG_BYTES {
        return Err(Error::Limit(format!(
            "{} exceeds the size limit",
            path.display()
        )));
    }
    let value: deser_value::Value = deser_json::from_slice(&bytes)
        .map_err(|error| Error::Integrity(format!("{} is not valid JSON: {error}", path.display())))?;
    let map = value
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{} must be a JSON object", path.display())))?;
    for key in map.keys() {
        if key.as_str() != Some("schemaVersion") && key.as_str() != Some("aliases") {
            return Err(Error::Integrity(format!(
                "{}: unknown field {:?}",
                path.display(),
                key.as_str().unwrap_or("<non-string>")
            )));
        }
    }
    let schema_version = map
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::Integrity(format!("{}: schemaVersion must be an integer", path.display())))?;
    if schema_version != 1 {
        return Err(Error::Integrity(format!(
            "{}: unsupported schemaVersion {schema_version}",
            path.display()
        )));
    }
    let mut out = FxHashMap::default();
    let Some(aliases) = map.get("aliases") else {
        return Ok(out);
    };
    let aliases = aliases
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{}: aliases must be an object", path.display())))?;
    for (repo_value, tags_value) in aliases.iter() {
        let repo = repo_value
            .as_str()
            .ok_or_else(|| Error::Integrity(format!("{}: aliases keys must be strings", path.display())))?
            .to_string();
        RepositoryName::parse(&repo)
            .map_err(|error| Error::Integrity(format!("{}: aliases key {repo:?}: {error}", path.display())))?;
        let tags = tags_value
            .as_map()
            .ok_or_else(|| Error::Integrity(format!("{}: aliases[{repo:?}] must be an object", path.display())))?;
        for (tag_value, record_value) in tags.iter() {
            let tag = tag_value
                .as_str()
                .ok_or_else(|| Error::Integrity(format!("{}: alias tags must be strings", path.display())))?;
            Tag::parse(tag)
                .map_err(|error| Error::Integrity(format!("{}: alias {tag:?}: {error}", path.display())))?;
            let record = record_value
                .as_map()
                .ok_or_else(|| Error::Integrity(format!("{}: aliases[{repo:?}][{tag:?}] must be an object", path.display())))?;
            for key in record.keys() {
                if key.as_str() != Some("digest") && key.as_str() != Some("entry") {
                    return Err(Error::Integrity(format!(
                        "{}: aliases[{repo:?}][{tag:?}]: unknown field {:?}",
                        path.display(),
                        key.as_str().unwrap_or("<non-string>")
                    )));
                }
            }
            let digest = Digest::parse(
                record
                    .get("digest")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| Error::Integrity(format!("{}: aliases[{repo:?}][{tag:?}]: digest must be a string", path.display())))?,
            )
            .map_err(|error| Error::Integrity(format!("{}: aliases[{repo:?}][{tag:?}]: {error}", path.display())))?;
            let entry = record
                .get("entry")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Integrity(format!("{}: aliases[{repo:?}][{tag:?}]: entry must be a string", path.display())))?
                .to_string();
            out.insert((repo.clone(), tag.to_string()), AliasOwnership { digest, entry });
        }
    }
    Ok(out)
}

pub fn render_ownership(records: &FxHashMap<(String, String), AliasOwnership>) -> String {
    let mut out = String::from("{\"schemaVersion\":1,\"aliases\":{");
    let mut by_repo: FxHashMap<&str, Vec<(&str, &AliasOwnership)>> = FxHashMap::default();
    for ((repo, tag), record) in records {
        by_repo.entry(repo).or_default().push((tag, record));
    }
    let mut repos: Vec<&str> = by_repo.keys().copied().collect();
    repos.sort_unstable();
    let mut first_repo = true;
    for repo in repos {
        let mut tags = by_repo.remove(repo).expect("collected above");
        tags.sort_by_key(|(tag, _)| *tag);
        if !first_repo {
            out.push(',');
        }
        first_repo = false;
        out.push_str(&format!("{}:{{", json_escape(repo)));
        for (index, (tag, record)) in tags.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{}:{{\"digest\":{},\"entry\":{}}}",
                json_escape(tag),
                json_escape(record.digest.as_str()),
                json_escape(&record.entry)
            ));
        }
        out.push('}');
    }
    out.push_str("}}");
    out
}

