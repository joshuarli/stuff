#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::json::json_escape;
use std::path::{Path, PathBuf};
use stuff_oci::Digest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheObject {
    pub digest: Digest,
    pub size: u64,
    pub media_type: String,
    pub role: ObjectRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectRole {
    Manifest,
    Config,
    Layer,
}

impl ObjectRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Config => "config",
            Self::Layer => "layer",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "manifest" => Some(Self::Manifest),
            "config" => Some(Self::Config),
            "layer" => Some(Self::Layer),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryCache {
    pub id: String,
    pub objects: Vec<CacheObject>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorCache {
    pub lock_digest: Digest,
    pub entries: Vec<EntryCache>,
}

impl MirrorCache {
    pub fn path_for(lock_path: &Path) -> PathBuf {
        PathBuf::from(format!("{}.cache", lock_path.display()))
    }

    pub fn load(lock_path: &Path) -> Result<Self> {
        let cache_path = Self::path_for(lock_path);
        let text = crate::mirror::inventory::read_capped(&cache_path, "lock cache")?;
        let cache = Self::parse(&text)?;
        let lock_digest = Digest::from_bytes(&std::fs::read(lock_path).map_err(|error| {
            Error::Operation(format!("could not read lock {}: {error}", lock_path.display()))
        })?);
        if cache.lock_digest != lock_digest {
            return Err(Error::Input(format!(
                "lock cache {} is stale (lock changed after resolution); rerun `myoci mirror resolve`",
                cache_path.display()
            )));
        }
        Ok(cache)
    }

    pub fn load_if_fresh(lock_path: &Path) -> Result<Option<Self>> {
        let cache_path = Self::path_for(lock_path);
        match std::fs::metadata(&cache_path) {
            Ok(_) => Ok(Some(Self::load(lock_path)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Operation(format!(
                "could not stat {}: {error}",
                cache_path.display()
            ))),
        }
    }

    pub fn render(&self) -> String {
        let mut entries = self.entries.clone();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        let mut out = String::from("{\"schema_version\":1,\"lock_digest\":");
        out.push_str(&json_escape(self.lock_digest.as_str()));
        out.push_str(",\"entries\":[");
        for (index, entry) in entries.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let mut objects = entry.objects.clone();
            objects.sort_by(|a, b| a.digest.cmp(&b.digest));
            objects.dedup_by(|a, b| a.digest == b.digest);
            let deduped: Vec<&CacheObject> = objects.iter().collect();
            out.push_str(&format!("{{\"id\":{},\"objects\":[", json_escape(&entry.id)));
            let rendered: Vec<String> = deduped
                .iter()
                .map(|object| {
                    format!(
                        "{{\"digest\":{},\"size\":{},\"media_type\":{},\"role\":{}}}",
                        json_escape(object.digest.as_str()),
                        object.size,
                        json_escape(&object.media_type),
                        json_escape(object.role.as_str())
                    )
                })
                .collect();
            out.push_str(&rendered.join(","));
            out.push_str("]}");
        }
        out.push_str("]}\n");
        out
    }

    pub fn parse(text: &str) -> Result<Self> {
        let value: deser_value::Value = deser_json::from_str(text)
            .map_err(|error| Error::Integrity(format!("lock cache is not valid JSON: {error}")))?;
        let map = value
            .as_map()
            .ok_or_else(|| Error::Integrity("lock cache must be a JSON object".to_string()))?;
        for key in map.keys() {
            match key.as_str().unwrap_or("") {
                "schema_version" | "lock_digest" | "entries" => {}
                other => {
                    return Err(Error::Integrity(format!(
                        "lock cache: unknown field {other:?}"
                    )))
                }
            }
        }
        let schema_version = map
            .get("schema_version")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| Error::Integrity("lock cache: schema_version must be an integer".to_string()))?;
        if schema_version != 1 {
            return Err(Error::Integrity(format!(
                "lock cache: unsupported schema_version {schema_version}"
            )));
        }
        let lock_digest = Digest::parse(
            map.get("lock_digest")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Integrity("lock cache: lock_digest must be a string".to_string()))?,
        )
        .map_err(|error| Error::Integrity(format!("lock cache: lock_digest: {error}")))?;
        let entries_value = map
            .get("entries")
            .and_then(|value| value.as_seq())
            .ok_or_else(|| Error::Integrity("lock cache: entries must be an array".to_string()))?;
        let mut entries = Vec::with_capacity(entries_value.len());
        for (index, entry_value) in entries_value.iter().enumerate() {
            entries.push(parse_entry(entry_value, index)?);
        }
        Ok(Self {
            lock_digest,
            entries,
        })
    }

    pub fn objects_for(&self, id: &str) -> Vec<CacheObject> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.objects.clone())
            .unwrap_or_default()
    }

    pub fn total_objects(&self) -> usize {
        self.entries.iter().map(|entry| entry.objects.len()).sum()
    }
}

fn parse_entry(value: &deser_value::Value, index: usize) -> Result<EntryCache> {
    let context = format!("lock cache entries[{index}]");
    let map = value
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("{context} must be an object")))?;
    for key in map.keys() {
        match key.as_str().unwrap_or("") {
            "id" | "objects" => {}
            other => {
                return Err(Error::Integrity(format!(
                    "{context}: unknown field {other:?}"
                )))
            }
        }
    }
    let id = map
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| Error::Integrity(format!("{context}: id must be a string")))?
        .to_string();
    let objects_value = map
        .get("objects")
        .and_then(|value| value.as_seq())
        .ok_or_else(|| Error::Integrity(format!("{context}: objects must be an array")))?;
    let mut objects = Vec::with_capacity(objects_value.len());
    for (object_index, object_value) in objects_value.iter().enumerate() {
        let octx = format!("{context} objects[{object_index}]");
        let object_map = object_value
            .as_map()
            .ok_or_else(|| Error::Integrity(format!("{octx} must be an object")))?;
        for key in object_map.keys() {
            match key.as_str().unwrap_or("") {
                "digest" | "size" | "media_type" | "role" => {}
                other => {
                    return Err(Error::Integrity(format!(
                        "{octx}: unknown field {other:?}"
                    )))
                }
            }
        }
        let digest = Digest::parse(
            object_map
                .get("digest")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Integrity(format!("{octx}: digest must be a string")))?,
        )
        .map_err(|error| Error::Integrity(format!("{octx} digest: {error}")))?;
        let size = object_map
            .get("size")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| Error::Integrity(format!("{octx}: size must be a non-negative integer")))?;
        let media_type = object_map
            .get("media_type")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Integrity(format!("{octx}: media_type must be a string")))?
            .to_string();
        let role = ObjectRole::parse(
            object_map
                .get("role")
                .and_then(|value| value.as_str())
                .ok_or_else(|| Error::Integrity(format!("{octx}: role must be a string")))?,
        )
        .ok_or_else(|| Error::Integrity(format!("{octx}: role is not recognized")))?;
        objects.push(CacheObject {
            digest,
            size,
            media_type,
            role,
        });
    }
    Ok(EntryCache { id, objects })
}

