#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;

use deser_value::Value;

use crate::publish::PublishError;

use super::client::ObjectMeta;
use super::multipart;
use super::put::{self, UploadOptions};

pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestObject {
    pub key: String,
    pub size: u64,
    pub etag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub objects: Vec<ManifestObject>,
}

pub fn build(
    site: &Path,
    keys: &[String],
    options: &UploadOptions,
    known: &BTreeMap<String, ObjectMeta>,
) -> Result<Manifest, PublishError> {
    let mut keys: Vec<String> = keys
        .iter()
        .filter(|key| !key.trim().is_empty())
        .map(|key| key.trim_end_matches('\n').to_string())
        .collect();
    keys.sort();
    let mut objects = Vec::with_capacity(keys.len());
    for key in keys {
        let path = manifest_site_path(site, &key)?;
        let (size, etag) = match std::fs::metadata(&path) {
            Ok(metadata) => (
                metadata.len(),
                multipart::etag_file(&path, options.single_put_max, options.part_size)?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let listed = known.get(&key).ok_or_else(|| {
                    PublishError::Integrity(format!("could not stat {}: {error}", path.display()))
                })?;
                (listed.size, listed.etag.clone())
            }
            Err(error) => {
                return Err(PublishError::Integrity(format!(
                    "could not stat {}: {error}",
                    path.display()
                )));
            }
        };
        objects.push(ManifestObject { key, size, etag });
    }
    Ok(Manifest { objects })
}

pub fn render(manifest: &Manifest) -> String {
    let mut out = String::from("{\n  \"objects\": [");
    if manifest.objects.is_empty() {
        out.push_str("],\n");
    } else {
        out.push('\n');
        for (index, object) in manifest.objects.iter().enumerate() {
            out.push_str("    {\n");
            out.push_str(&format!("      \"etag\": {},\n", json_string(&object.etag)));
            out.push_str(&format!("      \"key\": {},\n", json_string(&object.key)));
            out.push_str(&format!("      \"size\": {}\n", object.size));
            out.push_str("    }");
            if index + 1 < manifest.objects.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ],\n");
    }
    out.push_str(&format!("  \"schemaVersion\": {SCHEMA_VERSION}\n}}\n"));
    out
}

pub fn write(path: &Path, manifest: &Manifest) -> Result<(), PublishError> {
    std::fs::write(path, render(manifest)).map_err(|error| {
        PublishError::Integrity(format!("could not write {}: {error}", path.display()))
    })
}

pub fn parse(text: &str) -> Result<Manifest, PublishError> {
    let value: Value = deser_json::from_str(text)
        .map_err(|error| PublishError::Integrity(format!("manifest is invalid JSON: {error}")))?;
    let map = value
        .as_map()
        .ok_or_else(|| PublishError::Integrity("manifest has an unsupported shape".to_string()))?;
    let version = map
        .get("schemaVersion")
        .and_then(|value| value.kind().as_u64());
    if version != Some(SCHEMA_VERSION) {
        return Err(PublishError::Integrity(
            "manifest has an unsupported schemaVersion".to_string(),
        ));
    }
    let entries = map
        .get("objects")
        .and_then(|value| value.kind().as_seq())
        .ok_or_else(|| PublishError::Integrity("manifest has an unsupported shape".to_string()))?;
    let mut objects = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = entry.as_map().ok_or_else(|| {
            PublishError::Integrity("manifest contains a non-object entry".to_string())
        })?;
        let key = entry
            .get("key")
            .and_then(|value| value.kind().as_str())
            .ok_or_else(|| PublishError::Integrity("manifest entry has no key".to_string()))?;
        if !put::is_safe_key(key) {
            return Err(PublishError::Integrity(format!(
                "manifest contains an unsafe key: {key:?}"
            )));
        }
        let size = entry
            .get("size")
            .and_then(|value| value.kind().as_u64())
            .ok_or_else(|| {
                PublishError::Integrity(format!("manifest entry {key} has an invalid size"))
            })?;
        let etag = entry
            .get("etag")
            .and_then(|value| value.kind().as_str())
            .filter(|etag| !etag.is_empty())
            .ok_or_else(|| {
                PublishError::Integrity(format!("manifest entry {key} has an invalid ETag"))
            })?;
        objects.push(ManifestObject {
            key: key.to_string(),
            size,
            etag: etag.to_string(),
        });
    }
    Ok(Manifest { objects })
}

pub fn read(path: &Path) -> Result<Manifest, PublishError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        PublishError::Integrity(format!("could not read {}: {error}", path.display()))
    })?;
    parse(&text)
}

fn manifest_site_path(site: &Path, key: &str) -> Result<std::path::PathBuf, PublishError> {
    if !put::is_safe_key(key) {
        return Err(PublishError::Usage(format!("unsafe object key: {key:?}")));
    }
    Ok(site.join(key))
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_site(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (key, body) in files {
            let path = dir.path().join(key);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, body).unwrap();
        }
        dir
    }

    #[test]
    fn manifests_are_sorted_and_hash_small_and_large_files_alike() {
        let site = write_site(&[("b.txt", b"bbb"), ("a/catalog.json", b"{}")]);
        let options = UploadOptions {
            single_put_max: 2,
            part_size: 2,
            repair: false,
        };
        let manifest = build(
            site.path(),
            &["b.txt".to_string(), "a/catalog.json".to_string()],
            &options,
            &BTreeMap::new(),
        )
        .unwrap();
        let large_etag = multipart::multipart_etag_file(&site.path().join("b.txt"), 2).unwrap();
        assert_eq!(
            manifest.objects,
            vec![
                ManifestObject {
                    key: "a/catalog.json".to_string(),
                    size: 2,
                    etag: multipart::md5_hex(b"{}"),
                },
                ManifestObject {
                    key: "b.txt".to_string(),
                    size: 3,
                    etag: large_etag,
                },
            ]
        );
        let rendered = render(&manifest);
        assert!(rendered.contains("      \"etag\": "), "{rendered}");
        assert!(rendered.contains("      \"key\": \"a/catalog.json\",\n"), "{rendered}");
        assert_eq!(parse(&rendered).unwrap(), manifest);
        assert_eq!(
            render(&Manifest {
                objects: vec![ManifestObject {
                    key: "a/catalog.json".to_string(),
                    size: 2,
                    etag: "99914b932bd37a50b983c5e7c90ae93b".to_string(),
                }],
            }),
            "{\n  \"objects\": [\n    {\n      \"etag\": \"99914b932bd37a50b983c5e7c90ae93b\",\n      \"key\": \"a/catalog.json\",\n      \"size\": 2\n    }\n  ],\n  \"schemaVersion\": 1\n}\n"
        );
        assert_eq!(render(&Manifest { objects: Vec::new() }),
            "{\n  \"objects\": [],\n  \"schemaVersion\": 1\n}\n");
    }

    #[test]
    fn manifests_round_trip_through_json() {
        let site = write_site(&[("catalog.json", b"{}"), ("empty.txt", b"")]);
        let manifest = build(
            site.path(),
            &["catalog.json".to_string(), "empty.txt".to_string()],
            &UploadOptions::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        let text = render(&manifest);
        assert!(text.starts_with("{\n  \"objects\": [\n"));
        assert!(text.ends_with("  \"schemaVersion\": 1\n}\n"));
        assert!(!text.contains("\"schemaVersion\": 1,"));
        assert_eq!(parse(&text).unwrap(), manifest);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        write(&path, &manifest).unwrap();
        assert_eq!(read(&path).unwrap(), manifest);
    }

    #[test]
    fn parsing_rejects_unsupported_shapes() {
        for text in [
            "not json",
            "[]",
            r#"{"schemaVersion": 2, "objects": []}"#,
            r#"{"schemaVersion": 1}"#,
            r#"{"schemaVersion": 1, "objects": {}}"#,
            r#"{"schemaVersion": 1, "objects": [7]}"#,
            r#"{"schemaVersion": 1, "objects": [{"key": "/abs", "size": 1, "etag": "e"}]}"#,
            r#"{"schemaVersion": 1, "objects": [{"key": "a", "size": -1, "etag": "e"}]}"#,
            r#"{"schemaVersion": 1, "objects": [{"key": "a", "size": 1, "etag": ""}]}"#,
            r#"{"schemaVersion": 1, "objects": [{"key": "a", "size": 1}]}"#,
        ] {
            let error = parse(text).unwrap_err();
            assert_eq!(error.exit_code(), 4, "{text}: {error}");
        }
    }
}
