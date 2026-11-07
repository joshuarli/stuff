#![forbid(unsafe_code)]

use crate::archive::inspect_unidentified_package_archive;
use crate::error::{Error, Result};
use crate::inventory::{validate_artifact_record, ArtifactRecord};
use sha1::Sha1;
use sha2::{Digest, Sha512};
use std::io::Read;
use std::path::Path;

const COPIED_MANIFEST_FIELDS: [&str; 18] = [
    "description",
    "license",
    "type",
    "main",
    "module",
    "types",
    "exports",
    "sideEffects",
    "bin",
    "engines",
    "os",
    "cpu",
    "libc",
    "dependencies",
    "optionalDependencies",
    "peerDependencies",
    "peerDependenciesMeta",
    "bundleDependencies",
];

pub struct FirstPartyTimes<'a> {
    pub created: &'a str,
    pub published: &'a str,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn text(value: &str) -> deser_value::Value {
    deser_value::Value::from(value.to_string())
}

fn object(entries: Vec<(&str, deser_value::Value)>) -> deser_value::Value {
    let mut map = deser_value::Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), value);
    }
    deser_value::Value::from(map)
}

pub fn first_party_artifact(
    archive: &Path,
    public_base_url: &str,
    times: &FirstPartyTimes<'_>,
) -> Result<ArtifactRecord> {
    let path = archive.to_str().ok_or_else(|| Error::Input("package archive path is not UTF-8".to_string()))?;
    let inspected = inspect_unidentified_package_archive(path)?;
    let manifest = inspected
        .manifest
        .as_map()
        .ok_or_else(|| Error::Input("package archive package.json must contain an object".to_string()))?;
    let name = manifest.get("name").and_then(|value| value.as_str()).unwrap_or("").to_string();
    let version = manifest.get("version").and_then(|value| value.as_str()).unwrap_or("").to_string();

    let mut file = std::fs::File::open(archive)
        .map_err(|error| Error::Input(format!("could not read {}: {error}", archive.display())))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| Error::Input(format!("could not read {}: {error}", archive.display())))?;
    let sha512 = hex(&Sha512::digest(&bytes));
    let shasum = hex(&Sha1::digest(&bytes));
    let integrity = format!(
        "sha512-{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, Sha512::digest(&bytes))
    );
    let base = public_base_url.trim_end_matches('/');
    let tarball = format!("{base}/current/-/tarballs/{sha512}.tgz");

    let mut metadata = deser_value::Map::new();
    for field in COPIED_MANIFEST_FIELDS {
        if let Some(value) = manifest.get(field) {
            metadata.insert(field.to_string(), value.clone());
        }
    }
    metadata.insert("name".to_string(), text(&name));
    metadata.insert("version".to_string(), text(&version));
    metadata.insert(
        "dist".to_string(),
        object(vec![
            ("integrity", text(&integrity)),
            ("shasum", text(&shasum)),
            ("tarball", text(&tarball)),
        ]),
    );

    let packument = object(vec![
        ("name", text(&name)),
        ("dist-tags", object(vec![("latest", text(&version))])),
        (
            "time",
            object(vec![
                ("created", text(times.created)),
                ("modified", text(times.published)),
                (version.as_str(), text(times.published)),
            ]),
        ),
    ]);
    let artifact = object(vec![
        ("sha512", text(&sha512)),
        ("integrity", text(&integrity)),
        ("shasum", text(&shasum)),
        ("sourceUrl", text(&tarball)),
        ("legacySha1Only", deser_value::Value::from(false)),
        ("firstParty", deser_value::Value::from(true)),
    ]);
    let record = object(vec![
        ("name", text(&name)),
        ("version", text(&version)),
        ("packument", packument),
        ("metadata", deser_value::Value::from(metadata)),
        ("artifact", artifact),
    ]);
    validate_artifact_record(&record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::artifact_to_value;

    fn pack(dir: &Path, manifest: &str) -> std::path::PathBuf {
        let path = dir.join("pkg.tgz");
        let file = std::fs::File::create(&path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "package/package.json", manifest.as_bytes()).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    #[test]
    fn a_first_party_record_round_trips_and_a_foreign_source_url_stays_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = pack(dir.path(), r#"{"name":"@scope/ui","version":"0.1.0","scripts":{"build":"x"}}"#);
        let times = FirstPartyTimes { created: "2026-01-01T00:00:00.000Z", published: "2026-01-01T00:00:00.000Z" };
        let record = first_party_artifact(&path, "https://npm.example.test/", &times).unwrap();
        assert!(record.artifact.first_party);
        assert!(record.metadata.as_map().unwrap().get("scripts").is_none());

        let value = artifact_to_value(&record).unwrap();
        assert_eq!(validate_artifact_record(&value).unwrap(), record);

        let mut upstream = value.clone();
        upstream
            .as_map_mut()
            .unwrap()
            .get_mut("artifact")
            .unwrap()
            .as_map_mut()
            .unwrap()
            .remove("firstParty");
        assert!(validate_artifact_record(&upstream).is_err());
    }
}
