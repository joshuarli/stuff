#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use deser_value::Value;
use md5::{Digest as _, Md5};

use crate::publish::PublishError;

use super::client::{R2Client, RawRequest};
use super::sigv4;
use super::xml::ListedObject;

pub const PART_SIZE: u64 = 64 * 1024 * 1024;
pub const DOWNLOAD_ATTEMPTS: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullOptions {
    pub part_size: u64,
}

impl Default for PullOptions {
    fn default() -> Self {
        PullOptions { part_size: PART_SIZE }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullReport {
    pub generation: String,
    pub objects: usize,
    pub bytes: u64,
    pub verified: usize,
}

pub fn md5_hex(bytes: &[u8]) -> String {
    sigv4::hex(Md5::digest(bytes))
}

pub fn multipart_etag(bytes: &[u8], part_size: u64) -> String {
    let mut combined = Md5::new();
    let mut parts = 0_u64;
    if part_size > 0 {
        for chunk in bytes.chunks(part_size as usize) {
            combined.update(Md5::digest(chunk));
            parts += 1;
        }
    }
    format!("{}-{}", sigv4::hex(combined.finalize()), parts)
}

pub fn object_etag(bytes: &[u8], etag: &str, part_size: u64) -> String {
    if etag.contains('-') {
        multipart_etag(bytes, part_size)
    } else {
        md5_hex(bytes)
    }
}

pub fn safe_relative(key: &str) -> Result<PathBuf, PublishError> {
    let safe = !key.is_empty()
        && !key.starts_with('/')
        && key
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !safe {
        return Err(PublishError::Integrity(format!("unsafe object key: {key:?}")));
    }
    Ok(key.split('/').fold(PathBuf::new(), |mut path, part| {
        path.push(part);
        path
    }))
}

pub fn wanted(key: &str, generation: &str) -> bool {
    if key == "current" {
        return false;
    }
    if key.starts_with("generations/") {
        return key.starts_with(&format!("generations/{generation}/"));
    }
    if key.starts_with("manifests/") {
        return key == format!("manifests/{generation}.json");
    }
    true
}

pub fn is_generation_id(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn current_generation(client: &R2Client, bucket: &str) -> Result<String, PublishError> {
    let response = client.request(RawRequest {
        method: "GET",
        bucket,
        key: "current",
        query: Vec::new(),
        body: &[],
        content_type: None,
        extra_headers: Vec::new(),
    })?;
    if response.status != 200 {
        return Err(PublishError::Integrity(format!(
            "cannot read the current marker: HTTP {}",
            response.status
        )));
    }
    let generation = String::from_utf8_lossy(&response.body).trim().to_string();
    if !is_generation_id(&generation) {
        return Err(PublishError::Integrity(format!(
            "bad generation id in the current marker: {generation:?}"
        )));
    }
    Ok(generation)
}

pub fn list_all(client: &R2Client, bucket: &str) -> Result<Vec<ListedObject>, PublishError> {
    let mut objects = client.list_objects_v2(bucket)?;
    objects.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(objects)
}

pub fn download(
    client: &R2Client,
    bucket: &str,
    destination: &Path,
    object: &ListedObject,
    part_size: u64,
) -> Result<(), PublishError> {
    let path = destination.join(safe_relative(&object.key)?);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", parent.display()))
        })?;
    }
    let mut temporary = path.clone().into_os_string();
    temporary.push(".part");
    let temporary = PathBuf::from(temporary);
    let mut last = String::new();
    for _ in 0..DOWNLOAD_ATTEMPTS {
        let body = client.get_object(bucket, &object.key)?;
        if body.len() as u64 != object.size
            || object_etag(&body, &object.etag, part_size) != object.etag
        {
            last = "size or ETag differs from the listing".to_string();
            continue;
        }
        fs::write(&temporary, &body).map_err(|error| {
            PublishError::Config(format!("could not write {}: {error}", temporary.display()))
        })?;
        fs::rename(&temporary, &path).map_err(|error| {
            PublishError::Config(format!("could not rename {}: {error}", path.display()))
        })?;
        return Ok(());
    }
    Err(PublishError::Integrity(format!(
        "could not pull {}: {last}",
        object.key
    )))
}

pub fn verify_manifest(
    destination: &Path,
    generation: &str,
    part_size: u64,
) -> Result<usize, PublishError> {
    let manifest = destination
        .join("manifests")
        .join(format!("{generation}.json"));
    if !manifest.is_file() {
        eprintln!(
            "note: generation {generation} has no manifest; object verification is skipped"
        );
        return Ok(0);
    }
    let text = fs::read_to_string(&manifest).map_err(|error| {
        PublishError::Integrity(format!(
            "cannot read the manifest for generation {generation}: {error}"
        ))
    })?;
    let document: Value = deser_json::from_str(&text).map_err(|error| {
        PublishError::Integrity(format!(
            "cannot read the manifest for generation {generation}: {error}"
        ))
    })?;
    let unsupported = || {
        PublishError::Integrity(format!(
            "manifest for generation {generation} has an unsupported schema"
        ))
    };
    let map = document.as_map().ok_or_else(unsupported)?;
    if map.get("schemaVersion").and_then(|value| value.as_u64()) != Some(1) {
        return Err(unsupported());
    }
    let objects = map
        .get("objects")
        .and_then(|value| value.as_seq())
        .ok_or_else(|| {
            PublishError::Integrity(format!(
                "manifest for generation {generation} has no object list"
            ))
        })?;
    for entry in objects.iter() {
        let entry = entry.as_map().ok_or_else(|| {
            PublishError::Integrity(format!(
                "manifest for generation {generation} has an invalid object entry"
            ))
        })?;
        let key = entry.get("key").and_then(|value| value.as_str());
        let size = entry.get("size").and_then(|value| value.as_u64());
        let etag = entry.get("etag").and_then(|value| value.as_str());
        let (Some(key), Some(size), Some(etag)) = (key, size, etag) else {
            return Err(PublishError::Integrity(format!(
                "manifest for generation {generation} has an incomplete object entry"
            )));
        };
        let local = destination.join(safe_relative(key)?);
        let Ok(metadata) = fs::metadata(&local) else {
            return Err(PublishError::Integrity(format!(
                "manifest lists {key}, which was not pulled for generation {generation}"
            )));
        };
        if metadata.len() != size {
            return Err(PublishError::Integrity(format!(
                "{key} is {} bytes, but its manifest records {size} for generation {generation}",
                metadata.len()
            )));
        }
        let bytes = fs::read(&local).map_err(|error| {
            PublishError::Integrity(format!(
                "cannot read {key} for generation {generation}: {error}"
            ))
        })?;
        if object_etag(&bytes, etag, part_size) != etag {
            return Err(PublishError::Integrity(format!(
                "{key} differs from its manifest ETag for generation {generation}"
            )));
        }
    }
    Ok(objects.len())
}

pub fn pull(
    client: &R2Client,
    bucket: &str,
    destination: &Path,
    options: PullOptions,
) -> Result<PullReport, PublishError> {
    if destination.symlink_metadata().is_ok() {
        let empty = destination.is_dir()
            && fs::read_dir(destination)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
        if !empty {
            return Err(PublishError::Usage(format!(
                "{} exists and is not an empty directory",
                destination.display()
            )));
        }
    }
    let generation = current_generation(client, bucket)?;
    let objects: Vec<ListedObject> = list_all(client, bucket)?
        .into_iter()
        .filter(|object| wanted(&object.key, &generation))
        .collect();
    let generation_prefix = format!("generations/{generation}/");
    if !objects
        .iter()
        .any(|object| object.key.starts_with(&generation_prefix))
    {
        return Err(PublishError::Integrity(format!(
            "generation {generation} has no objects in {bucket}"
        )));
    }
    fs::create_dir_all(destination).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            destination.display()
        ))
    })?;
    for object in &objects {
        download(client, bucket, destination, object, options.part_size)?;
    }
    let verified = verify_manifest(destination, &generation, options.part_size)?;
    let current = destination.join("current");
    std::os::unix::fs::symlink(format!("generations/{generation}"), &current).map_err(|error| {
        PublishError::Config(format!("could not link {}: {error}", current.display()))
    })?;
    let wheels_store = destination.join("wheels");
    let wheels_alias = destination
        .join("generations")
        .join(&generation)
        .join("wheels");
    if wheels_store.is_dir() && wheels_alias.symlink_metadata().is_err() {
        std::os::unix::fs::symlink("../../wheels", &wheels_alias).map_err(|error| {
            PublishError::Config(format!("could not link {}: {error}", wheels_alias.display()))
        })?;
    }
    let bytes = objects.iter().map(|object| object.size).sum();
    Ok(PullReport {
        generation,
        objects: objects.len(),
        bytes,
        verified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_part_etags_are_the_bare_md5() {
        assert_eq!(md5_hex(b"body"), "841a2d689ad86bd1611447453c22c6fc");
        assert_eq!(
            object_etag(b"body", "841a2d689ad86bd1611447453c22c6fc", 4),
            "841a2d689ad86bd1611447453c22c6fc"
        );
    }

    #[test]
    fn multipart_etags_match_the_python_oracle() {
        assert_eq!(
            multipart_etag(b"abcdefgh", 4),
            "cb93ad6c9c920e2602b79a11ded63ddb-2"
        );
        assert_eq!(
            object_etag(b"abcdefgh", "cb93ad6c9c920e2602b79a11ded63ddb-2", 4),
            "cb93ad6c9c920e2602b79a11ded63ddb-2"
        );
    }

    #[test]
    fn unsafe_object_keys_are_rejected() {
        for key in ["", "/absolute", "a//b", "a/./b", "a/../b", ".."] {
            let error = safe_relative(key).expect_err(key);
            assert_eq!(error.exit_code(), 4, "{key}");
        }
        assert_eq!(safe_relative("blobs/a.crate").unwrap(), PathBuf::from("blobs/a.crate"));
    }

    #[test]
    fn only_the_current_generation_and_its_manifest_are_wanted() {
        assert!(!wanted("current", "g"));
        assert!(wanted("generations/g/catalog.json", "g"));
        assert!(!wanted("generations/old/catalog.json", "g"));
        assert!(wanted("manifests/g.json", "g"));
        assert!(!wanted("manifests/old.json", "g"));
        assert!(wanted("blobs/a.crate", "g"));
        assert!(wanted("tarballs/a.tgz", "g"));
        assert!(wanted("wheels/a.whl", "g"));
    }

    #[test]
    fn generation_ids_use_the_documented_alphabet() {
        assert!(is_generation_id("g1"));
        assert!(is_generation_id("aZ0_-"));
        assert!(!is_generation_id(""));
        assert!(!is_generation_id("has space"));
        assert!(!is_generation_id("slash/"));
        assert!(!is_generation_id("dot."));
    }
}
