#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::inventory::{canonical_json, validate_artifact_record, ArtifactRecord};

pub const RENDERER_VERSION: u32 = 4;

#[derive(Debug, Clone)]
pub struct PackumentFile {
    pub name: String,
    pub path: String,
    pub route: String,
    pub bytes: Vec<u8>,
}

fn validate_base_url(value: &str) -> Result<String> {
    let url = url::Url::parse(value)
        .map_err(|_| Error::Input("publicBaseUrl must be an absolute HTTPS URL".to_string()))?;
    let host = url.host_str().unwrap_or("");
    let is_loopback = url.scheme() == "http"
        && matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if url.scheme() != "https" && !is_loopback {
        return Err(Error::Input(
            "publicBaseUrl must be an HTTPS URL or a loopback HTTP URL with a trailing slash and no credentials, query, or fragment".to_string(),
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with('/')
    {
        return Err(Error::Input(
            "publicBaseUrl must be an HTTPS URL or a loopback HTTP URL with a trailing slash and no credentials, query, or fragment".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn latest_snapshot(records: &[ArtifactRecord]) -> Result<&ArtifactRecord> {
    let mut ordered: Vec<&ArtifactRecord> = records.iter().collect();
    ordered.sort_by(|a, b| {
        packument_modified(a).cmp(&packument_modified(b)).then_with(|| {
            canonical_json(&a.packument)
                .unwrap_or_default()
                .cmp(&canonical_json(&b.packument).unwrap_or_default())
        })
    });
    ordered
        .into_iter()
        .last()
        .ok_or_else(|| Error::Corrupt("packument has no versions".to_string()))
}

fn packument_modified(record: &ArtifactRecord) -> String {    record
        .packument
        .as_map()
        .and_then(|m| m.get("modified"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn dist_latest_tags(records: &[ArtifactRecord]) -> std::collections::BTreeMap<String, String> {
    let mut grouped: std::collections::BTreeMap<&str, Vec<&ArtifactRecord>> =
        std::collections::BTreeMap::new();
    for record in records {
        grouped.entry(record.name.as_str()).or_default().push(record);
    }
    let mut out = std::collections::BTreeMap::new();
    for (name, group) in grouped {
        let owned: Vec<ArtifactRecord> = group.into_iter().cloned().collect();
        let Ok(snapshot) = latest_snapshot(&owned) else {
            continue;
        };
        let latest = snapshot
            .packument
            .as_map()
            .and_then(|m| m.get("dist-tags"))
            .and_then(|v| v.as_map())
            .and_then(|m| m.get("latest"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(tag) = latest
            && owned.iter().any(|r| r.version == tag) {
                out.insert(name.to_string(), tag);
            }
    }
    out
}

fn package_time(
    records: &[ArtifactRecord],
    selected: &std::collections::BTreeSet<String>,
) -> Result<Option<deser_value::Value>> {
    let mut values: std::collections::BTreeMap<String, deser_value::Value> = Default::default();
    for record in records {
        let time = record
            .packument
            .as_map()
            .and_then(|m| m.get("time"))
            .and_then(|v| v.as_map());
        let Some(time) = time else { continue };
        for version in selected {
            if let Some(tv) = time.get(version.as_str()) {
                if let Some(prev) = values.get(version)
                    && canonical_json(prev).unwrap_or_default() != canonical_json(tv).unwrap_or_default() {
                        return Err(Error::Corrupt(format!(
                            "upstream publish time conflicts for {}@{}",
                            record.name, record.version
                        )));
                    }
                values.insert(version.clone(), tv.clone());
            }
        }
    }
    if values.is_empty() {
        return Ok(None);
    }
    let mut map = deser_value::Map::new();
    for (k, v) in values {
        map.insert(k, v);
    }
    Ok(Some(deser_value::Value::from(map)))
}

fn version_document(record: &ArtifactRecord, public_base_url: &str, renderer: u32) -> Result<deser_value::Value> {
    let mut metadata = record.metadata.clone();
    let mmap = metadata.as_map_mut().ok_or_else(|| {
        Error::Corrupt(format!("verified metadata is invalid for {}@{}", record.name, record.version))
    })?;
    let dist_val = mmap.get("dist").cloned().unwrap_or_else(|| deser_value::Value::from(deser_value::Kind::Null));
    let mut dist = dist_val.as_map().cloned().ok_or_else(|| {
        Error::Corrupt(format!(
            "verified distribution metadata is invalid for {}@{}",
            record.name, record.version
        ))
    })?;
    dist.remove("signatures");
    dist.remove("attestations");
    let tarball_path = match renderer {
        1 => "tarballs",
        2 => "current/tarballs",
        _ => "current/-/tarballs",
    };
    let base = public_base_url.trim_end_matches('/');
    let tarball = format!("{base}/{tarball_path}/{}.tgz", record.artifact.sha512);
    dist.insert("tarball".to_string(), deser_value::Value::from(tarball));
    dist.insert(
        "integrity".to_string(),
        deser_value::Value::from(record.artifact.integrity.clone()),
    );
    match &record.artifact.shasum {
        None => {
            dist.remove("shasum");
        }
        Some(s) => {
            dist.insert("shasum".to_string(), deser_value::Value::from(s.clone()));
        }
    }
    let name = mmap.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let version = mmap.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if name.is_empty() || version.is_empty() {
        return Err(Error::Corrupt(format!(
            "verified metadata identity is invalid for {}@{}",
            record.name, record.version
        )));
    }
    mmap.insert("dist".to_string(), deser_value::Value::from(dist));
    let mut out = deser_value::Map::new();
    if let Some(m) = metadata.as_map() {
        for i in 0..m.len() {
            if let Some((k, v)) = m.get_index(i) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out.insert("name".to_string(), deser_value::Value::from(name));
    out.insert("version".to_string(), deser_value::Value::from(version));
    Ok(deser_value::Value::from(out))
}

fn render_package(
    name: &str,
    records: &[ArtifactRecord],
    public_base_url: &str,
    renderer: u32,
) -> Result<deser_value::Value> {    let mut versions = deser_value::Map::new();
    let mut selected = std::collections::BTreeSet::new();
    for record in records {
        selected.insert(record.version.clone());
        versions.insert(record.version.clone(), version_document(record, public_base_url, renderer)?);
    }
    let snapshot = latest_snapshot(records)?;
    let mut out = deser_value::Map::new();
    if let Some(snap) = snapshot.packument.as_map() {
        for i in 0..snap.len() {
            if let Some((k, v)) = snap.get_index(i) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out.insert("name".to_string(), deser_value::Value::from(name.to_string()));
    let mut dist_tags = deser_value::Map::new();
    if let Some(tags) = snapshot
        .packument
        .as_map()
        .and_then(|m| m.get("dist-tags"))
        .and_then(|v| v.as_map())
    {
        for i in 0..tags.len() {
            if let Some((tag, ver)) = tags.get_index(i)
                && let Some(vs) = ver.as_str()
                && selected.contains(vs) {
                    let tag_name = tag.as_str().unwrap_or("").to_string();
                    dist_tags.insert(tag_name, deser_value::Value::from(vs.to_string()));
                }
        }
    }
    out.insert("dist-tags".to_string(), deser_value::Value::from(dist_tags));
    out.insert("versions".to_string(), deser_value::Value::from(versions));
    match package_time(records, &selected)? {
        Some(mut time) => {
            if let (Some(tmap), Some(snap)) = (time.as_map_mut(), snapshot.packument.as_map())
                && let Some(snap_time) = snap.get("time").and_then(|v| v.as_map()) {
                    for field in ["created", "modified"] {
                        if let Some(fv) = snap_time.get(field) {
                            tmap.insert(field.to_string(), fv.clone());
                        }
                    }
                }
            out.insert("time".to_string(), time);
        }
        None => {
            out.remove("time");
        }
    }
    Ok(deser_value::Value::from(out))
}

pub fn render_packument_files(
    artifacts: &[deser_value::Value],
    public_base_url: &str,
    renderer: Option<u32>,
) -> Result<Vec<PackumentFile>> {
    let renderer = renderer.unwrap_or(RENDERER_VERSION);
    if renderer != 1 && renderer != 2 && renderer != 3 && renderer != 4 {
        return Err(Error::Input(format!("unsupported renderer version: {renderer}")));
    }
    let base = validate_base_url(public_base_url)?;
    let mut records = Vec::with_capacity(artifacts.len());
    for value in artifacts {
        records.push(validate_artifact_record(value)?);
    }
    records.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    let identities: Vec<String> = records.iter().map(|r| format!("{}\0{}", r.name, r.version)).collect();
    let mut dedup = identities.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != identities.len() {
        return Err(Error::Conflict("verified artifacts contain duplicate package identities".to_string()));
    }
    let mut grouped: std::collections::BTreeMap<String, Vec<ArtifactRecord>> = Default::default();
    for record in records {
        grouped.entry(record.name.clone()).or_default().push(record);
    }
    let mut files = Vec::new();
    for (name, package_records) in grouped {
        let rendered = render_package(&name, &package_records, &base, renderer)?;
        let mut sorted = rendered;
        crate::inventory::sort_canonical_value(&mut sorted);
        let pretty = format!("{}\n", crate::inventory::render_pretty(&sorted));
        files.push(PackumentFile {
            path: name.clone(),
            route: if let Some(i) = name.find('/') {
                format!("{}%2f{}", &name[..i], &name[i + 1..])
            } else {
                name.clone()
            },
            name,
            bytes: pretty.into_bytes(),
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serde_json_like(name: &str, version: &str, sha512: &str) -> deser_value::Value {
        use base64::Engine;
        let raw: Vec<u8> = (0..sha512.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&sha512[i..i + 2], 16).unwrap())
            .collect();
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(&raw)
        );
        let text = format!(
            "{{\"name\":\"{name}\",\"version\":\"{version}\",\"packument\":{{\"name\":\"{name}\",\"dist-tags\":{{\"latest\":\"{version}\"}},\"time\":{{\"modified\":\"2026-01-01T00:00:00.000Z\"}}}},\"metadata\":{{\"name\":\"{name}\",\"version\":\"{version}\",\"dist\":{{\"tarball\":\"https://registry.npmjs.org/{name}/-/{version}.tgz\"}}}},\"artifact\":{{\"sha512\":\"{sha512}\",\"integrity\":\"{integrity}\",\"shasum\":null,\"sourceUrl\":\"https://registry.npmjs.org/{name}/-/{version}.tgz\",\"legacySha1Only\":false}}}}"
        );
        deser_json::from_str(&text).unwrap()
    }

    #[test]
    fn renders_packuments_with_scoped_routes() {
        let zeros = "0".repeat(128);
        let scoped = serde_json_like("@scope/pkg", "1.0.0", &zeros);
        let flat = serde_json_like("flatdemo", "1.2.3", &zeros);
        let files = render_packument_files(&[flat, scoped], "https://example.test/mirror/", None).unwrap();
        assert_eq!(files.len(), 2);
        let scoped_file = files.iter().find(|f| f.name == "@scope/pkg").unwrap();
        assert_eq!(scoped_file.route, "@scope%2fpkg");
        assert_eq!(scoped_file.path, "@scope/pkg");
        let text = String::from_utf8(scoped_file.bytes.clone()).unwrap();
        assert!(text.contains(&format!("https://example.test/mirror/current/-/tarballs/{zeros}.tgz")));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn rejects_bad_base_and_duplicates() {
        let zeros = "0".repeat(128);
        let flat = serde_json_like("flatdemo", "1.2.3", &zeros);
        assert!(render_packument_files(std::slice::from_ref(&flat), "http://example.test/mirror/", None).is_err());
        assert!(render_packument_files(&[flat.clone(), flat], "https://example.test/mirror/", None).is_err());
        assert!(render_packument_files(&[], "https://example.test/mirror/", Some(9)).is_err());
    }
}
