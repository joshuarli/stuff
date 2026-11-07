#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::publish::PublishError;

use super::client::{ObjectMeta, Precondition, R2Client};
use super::multipart;

pub fn content_type(key: &str) -> &'static str {
    if key.ends_with(".json") {
        "application/json"
    } else if key.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if key.ends_with(".txt") || key.ends_with(".md") {
        "text/markdown; charset=utf-8"
    } else if key.ends_with(".toml") {
        "text/plain; charset=utf-8"
    } else if key.ends_with(".crate") {
        "application/x-tar"
    } else if key.ends_with(".tgz") || key.ends_with(".whl") || key.ends_with(".gz") {
        "application/octet-stream"
    } else if key.ends_with(".metadata") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

pub fn is_safe_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && key
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutAction {
    Skip,
    Put,
    Multipart,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutEntry {
    pub key: String,
    pub size: u64,
    pub action: PutAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOptions {
    pub single_put_max: u64,
    pub part_size: u64,
    pub repair: bool,
}

impl Default for UploadOptions {
    fn default() -> Self {
        UploadOptions {
            single_put_max: multipart::DEFAULT_SINGLE_PUT_MAX,
            part_size: multipart::PART_SIZE,
            repair: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PutReport {
    pub uploaded: Vec<String>,
    pub skipped: Vec<String>,
    pub multipart: Vec<String>,
}

pub fn plan_key(
    size: u64,
    local_etag: &str,
    remote: Option<&ObjectMeta>,
    single_put_max: u64,
) -> PutAction {
    let large = size > single_put_max;
    match remote {
        Some(found) if found.size == size && found.etag == local_etag => PutAction::Skip,
        Some(_) => PutAction::Conflict,
        None if large => PutAction::Multipart,
        None => PutAction::Put,
    }
}

pub fn plan(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    keys: &[String],
    options: &UploadOptions,
) -> Result<Vec<PutEntry>, PublishError> {
    let remote = remote_index(client, bucket, keys)?;
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
        let path = site_path(site, key)?;
        let size = std::fs::metadata(&path)
            .map_err(|error| {
                PublishError::Integrity(format!("could not stat {}: {error}", path.display()))
            })?
            .len();
        let local = multipart::etag_file(&path, options.single_put_max, options.part_size)?;
        let mut action = plan_key(size, &local, remote.get(key), options.single_put_max);
        if action == PutAction::Conflict && options.repair {
            action = if size > options.single_put_max {
                PutAction::Multipart
            } else {
                PutAction::Put
            };
        }
        entries.push(PutEntry {
            key: key.clone(),
            size,
            action,
        });
    }
    Ok(entries)
}

pub fn put_objects(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    keys: &[String],
    options: &UploadOptions,
) -> Result<PutReport, PublishError> {
    let mut report = PutReport::default();
    for key in keys {
        upload_key(client, site, bucket, key, options, &mut report)?;
    }
    Ok(report)
}

pub fn sync(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    keys: &[String],
    options: &UploadOptions,
) -> Result<PutReport, PublishError> {
    let entries = plan(client, site, bucket, keys, options)?;
    if let Some(conflict) = entries
        .iter()
        .find(|entry| entry.action == PutAction::Conflict)
    {
        return Err(PublishError::Conflict(format!(
            "immutable object differs: {}",
            conflict.key
        )));
    }
    let mut report = PutReport::default();
    for entry in &entries {
        match entry.action {
            PutAction::Skip => report.skipped.push(entry.key.clone()),
            PutAction::Conflict => unreachable!("conflicts were rejected before upload"),
            PutAction::Put => {
                upload_single(client, site, bucket, &entry.key)?;
                report.uploaded.push(entry.key.clone());
            }
            PutAction::Multipart => {
                upload_multipart(client, site, bucket, &entry.key, options)?;
                report.multipart.push(entry.key.clone());
                report.uploaded.push(entry.key.clone());
            }
        }
    }
    Ok(report)
}

fn remote_index(
    client: &R2Client,
    bucket: &str,
    keys: &[String],
) -> Result<HashMap<String, ObjectMeta>, PublishError> {
    let wanted: std::collections::HashSet<&str> =
        keys.iter().map(String::as_str).collect();
    let mut index = HashMap::with_capacity(wanted.len());
    for object in client.list_objects_v2(bucket)? {
        if wanted.contains(object.key.as_str()) {
            index.insert(
                object.key,
                ObjectMeta {
                    size: object.size,
                    etag: object.etag,
                    content_type: None,
                },
            );
        }
    }
    Ok(index)
}

fn upload_key(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    key: &str,
    options: &UploadOptions,
    report: &mut PutReport,
) -> Result<(), PublishError> {
    let path = site_path(site, key)?;
    let size = std::fs::metadata(&path)
        .map_err(|error| {
            PublishError::Integrity(format!("could not stat {}: {error}", path.display()))
        })?
        .len();
    if size > options.single_put_max {
        upload_multipart(client, site, bucket, key, options)?;
        report.multipart.push(key.to_string());
    } else {
        upload_single(client, site, bucket, key)?;
    }
    report.uploaded.push(key.to_string());
    Ok(())
}

fn upload_single(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    key: &str,
) -> Result<(), PublishError> {
    let path = site_path(site, key)?;
    let body = std::fs::read(&path).map_err(|error| {
        PublishError::Integrity(format!("could not read {}: {error}", path.display()))
    })?;
    client.put_object(bucket, key, &body, content_type(key), Precondition::None)?;
    Ok(())
}

fn upload_multipart(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    key: &str,
    options: &UploadOptions,
) -> Result<(), PublishError> {
    let path = site_path(site, key)?;
    multipart::upload_file(
        client,
        bucket,
        key,
        &path,
        content_type(key),
        options.part_size,
    )?;
    Ok(())
}

fn site_path(site: &Path, key: &str) -> Result<PathBuf, PublishError> {
    if !is_safe_key(key) {
        return Err(PublishError::Usage(format!("unsafe object key: {key:?}")));
    }
    Ok(site.join(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_match_the_site_layout() {
        for (key, kind) in [
            ("catalog.json", "application/json"),
            ("index.html", "text/html; charset=utf-8"),
            ("llms.txt", "text/markdown; charset=utf-8"),
            ("README.md", "text/markdown; charset=utf-8"),
            ("pyproject.toml", "text/plain; charset=utf-8"),
            ("blobs/a.crate", "application/x-tar"),
            ("tarballs/a.tgz", "application/octet-stream"),
            ("wheels/a.whl", "application/octet-stream"),
            ("blobs/a.gz", "application/octet-stream"),
            ("meta.metadata", "text/plain; charset=utf-8"),
            ("other.bin", "application/octet-stream"),
        ] {
            assert_eq!(content_type(key), kind, "{key}");
        }
    }

    #[test]
    fn unsafe_keys_are_rejected() {
        for key in ["", "/absolute", "a//b", "a/./b", "a/../b"] {
            assert!(!is_safe_key(key), "{key}");
        }
        for key in ["a", "a/b", "generations/g1/catalog.json"] {
            assert!(is_safe_key(key), "{key}");
        }
    }

    #[test]
    fn plan_decisions_compare_size_and_content_hash() {
        let remote = ObjectMeta {
            size: 3,
            etag: multipart::md5_hex(b"abc"),
            content_type: None,
        };
        assert_eq!(
            plan_key(3, &multipart::md5_hex(b"abc"), Some(&remote), 300),
            PutAction::Skip
        );
        assert_eq!(
            plan_key(3, &multipart::md5_hex(b"abd"), Some(&remote), 300),
            PutAction::Conflict
        );
        assert_eq!(
            plan_key(4, &multipart::md5_hex(b"abcd"), Some(&remote), 300),
            PutAction::Conflict
        );
        assert_eq!(
            plan_key(3, &multipart::md5_hex(b"abc"), None, 300),
            PutAction::Put
        );
        assert_eq!(
            plan_key(3, &multipart::md5_hex(b"abc"), None, 2),
            PutAction::Multipart
        );
    }
}
