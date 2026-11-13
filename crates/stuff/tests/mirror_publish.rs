#![forbid(unsafe_code)]

mod support;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mymirror::{Acquire, Error};
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mirror";
const UPSTREAM: &str = "https://upstream.example.test";

fn environment(work: &Path, fake: &FakeS3) -> stuff::publish::Environment {
    let mut pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| !name.starts_with("STUFF_") && !name.starts_with("R2_"))
        .collect();
    for (name, value) in [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CACHE_HOME", work.join("stuff-cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("stuff-state").display().to_string()),
        ("STUFF_CONFIG_HOME", work.join("stuff-config").display().to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    stuff::publish::Environment::from_pairs(pairs)
}

fn tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        header.set_entry_type(tar::EntryType::file());
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

fn fixtures(work: &Path) -> PathBuf {
    let directory = work.join("fixtures");
    fs::create_dir_all(&directory).unwrap();
    let archive = tar_gz(&[
        ("pkg-1.0.0/bin/app", b"#!/bin/sh\nexit 0\n", 0o755),
        ("pkg-1.0.0/bin/helper", b"helper", 0o644),
        ("pkg-1.0.0/docs/guide.txt", b"guide", 0o644),
        ("pkg-1.0.0/share/ignored.txt", b"ignored", 0o644),
    ]);
    fs::write(directory.join("app.tar.gz"), archive).unwrap();
    fs::write(directory.join("install.sh"), b"#!/bin/sh\necho install\n").unwrap();
    directory
}

fn sha256_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).unwrap();
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn setup(work: &Path) -> PathBuf {
    let directory = fixtures(work);
    let manifest = work.join("manifest.toml");
    let app_sha = sha256_file(&directory.join("app.tar.gz"));
    let script_sha = sha256_file(&directory.join("install.sh"));
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\n\
             \n\
             [[artifact]]\n\
             id = \"app\"\n\
             version = \"1.0.0\"\n\
             url = \"{UPSTREAM}/app.tar.gz\"\n\
             sha256 = \"{app_sha}\"\n\
             strip_components = 1\n\
             keep = [\"bin/**\", \"docs/**\"]\n\
             \n\
             [[artifact]]\n\
             id = \"tool-install\"\n\
             version = \"2026-11-13\"\n\
             url = \"{UPSTREAM}/install.sh\"\n\
             sha256 = \"{script_sha}\"\n"
        ),
    )
    .unwrap();
    manifest
}

struct FakeAcquire {
    responses: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<String>>,
}

impl Acquire for FakeAcquire {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.requests.lock().unwrap().push(url.to_string());
        self.responses
            .get(url)
            .cloned()
            .ok_or_else(|| Error::Transport(format!("no fake response for {url}")))
    }
}

fn acquire(work: &Path) -> FakeAcquire {
    let directory = work.join("fixtures");
    FakeAcquire {
        responses: [
            (format!("{UPSTREAM}/app.tar.gz"), fs::read(directory.join("app.tar.gz")).unwrap()),
            (format!("{UPSTREAM}/install.sh"), fs::read(directory.join("install.sh")).unwrap()),
        ]
        .into_iter()
        .collect(),
        requests: Mutex::new(Vec::new()),
    }
}

fn publish(
    manifest: &Path,
    dry_run: bool,
    allow_drops: bool,
    environment: &stuff::publish::Environment,
    acquire: &FakeAcquire,
) -> Result<stuff::publish::Outcome, stuff::publish::PublishError> {
    stuff::publish::mirror::publish_with_acquire(
        &stuff::publish::MirrorPublish {
            manifest: Some(manifest.to_string_lossy().into_owned()),
            allow_drops,
        },
        stuff::publish::Options {
            dry_run,
            ..stuff::publish::Options::default()
        },
        environment,
        acquire,
    )
}

fn remote_body(fake: &FakeS3, key: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == BUCKET && object.key == key)
        .map(|object| object.body)
}

fn remote_keys(fake: &FakeS3) -> Vec<String> {
    let mut keys: Vec<String> = fake
        .state()
        .into_iter()
        .filter(|object| object.bucket == BUCKET)
        .map(|object| object.key)
        .collect();
    keys.sort();
    keys
}

#[test]
fn publish_builds_the_mirror_and_is_idempotent() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let manifest = setup(work.path());
    let environment = environment(work.path(), &fake);
    let first = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap();
    assert_eq!(first.status, stuff::publish::Status::Published, "{first:?}");
    assert_eq!(first.registry, stuff::publish::Registry::Mirror);
    assert_eq!(first.packages, vec!["app", "tool-install"]);

    let generation = String::from_utf8(remote_body(&fake, "current").unwrap()).unwrap();
    let prefix = format!("generations/{generation}/");
    let keys = remote_keys(&fake);
    for expected in [
        format!("{prefix}app.zip"),
        format!("{prefix}tool-install.zip"),
        format!("{prefix}catalog.json"),
        format!("{prefix}SHA256SUMS"),
        format!("manifests/{generation}.json"),
        "current".to_string(),
    ] {
        assert!(keys.contains(&expected), "missing {expected} in {keys:?}");
    }
    let catalog =
        String::from_utf8(remote_body(&fake, &format!("{prefix}catalog.json")).unwrap()).unwrap();
    assert!(catalog.contains("\"registry\": \"mirror\""), "{catalog}");
    assert!(catalog.contains("\"name\": \"app\""), "{catalog}");

    let sums = String::from_utf8(remote_body(&fake, &format!("{prefix}SHA256SUMS")).unwrap()).unwrap();
    for line in sums.lines() {
        let digest = line.split_whitespace().next().unwrap();
        assert_eq!(digest.len(), 64);
    }

    let second = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap();
    assert_eq!(second.status, stuff::publish::Status::Unchanged, "{second:?}");
}

#[test]
fn a_dry_run_verifies_without_uploading() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let manifest = setup(work.path());
    let environment = environment(work.path(), &fake);
    let outcome = publish(&manifest, true, false, &environment, &acquire(work.path())).unwrap();
    assert!(outcome.dry_run);
    assert_eq!(remote_keys(&fake), Vec::<String>::new());
}

#[test]
fn bumping_an_artifact_version_requires_allow_drops() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let manifest = setup(work.path());
    let environment = environment(work.path(), &fake);
    let first = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap();
    assert_eq!(first.status, stuff::publish::Status::Published);

    let bumped = fs::read_to_string(&manifest)
        .unwrap()
        .replacen("version = \"1.0.0\"", "version = \"1.0.1\"", 1);
    fs::write(&manifest, bumped).unwrap();
    let error = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap_err();
    assert_eq!(error.classification(), stuff::publish::ExitCode::Conflict, "{error}");

    let accepted = publish(&manifest, false, true, &environment, &acquire(work.path())).unwrap();
    assert_eq!(accepted.status, stuff::publish::Status::Published, "{accepted:?}");
}

struct PanickingAcquire;

impl Acquire for PanickingAcquire {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, Error> {
        panic!("unexpected upstream request {url}");
    }
}

#[test]
fn an_unchanged_mirror_does_not_touch_the_upstream() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let manifest = setup(work.path());
    let environment = environment(work.path(), &fake);
    let first = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap();
    assert_eq!(first.status, stuff::publish::Status::Published);
    let rebuilt = stuff::publish::mirror::publish_with_acquire(
        &stuff::publish::MirrorPublish {
            manifest: Some(manifest.to_string_lossy().into_owned()),
            allow_drops: false,
        },
        stuff::publish::Options::default(),
        &environment,
        &PanickingAcquire,
    )
    .unwrap();
    assert_eq!(rebuilt.status, stuff::publish::Status::Unchanged, "{rebuilt:?}");
}

#[test]
fn republish_rebuilds_from_the_manifest() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let manifest = setup(work.path());
    let environment = environment(work.path(), &fake);
    let first = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap();
    assert_eq!(first.status, stuff::publish::Status::Published);
    let rebuilt = stuff::publish::mirror::republish_with_acquire(
        false,
        Some(manifest.to_string_lossy().into_owned()),
        &stuff::publish::Options::default(),
        &environment,
        &acquire(work.path()),
    )
    .unwrap();
    assert_eq!(rebuilt.status, stuff::publish::Status::Unchanged, "{rebuilt:?}");
}

#[test]
fn the_upstream_bytes_are_verified_before_anything_is_uploaded() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    fixtures(work.path());
    let manifest = work.path().join("manifest.toml");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\n\n[[artifact]]\nid = \"app\"\nversion = \"1.0.0\"\nurl = \"{UPSTREAM}/app.tar.gz\"\nsha256 = \"{}\"\n",
            "0".repeat(64)
        ),
    )
    .unwrap();
    let environment = environment(work.path(), &fake);
    let error = publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap_err();
    assert_eq!(error.classification(), stuff::publish::ExitCode::Integrity, "{error}");
    assert_eq!(remote_keys(&fake), Vec::<String>::new());
}

#[test]
fn the_zip_members_survive_the_round_trip() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let manifest = setup(work.path());
    let environment = environment(work.path(), &fake);
    publish(&manifest, false, false, &environment, &acquire(work.path())).unwrap();
    let generation = String::from_utf8(remote_body(&fake, "current").unwrap()).unwrap();
    let zip = remote_body(&fake, &format!("generations/{generation}/app.zip")).unwrap();
    let archive = rawzip::ZipArchive::from_slice(&zip).unwrap();
    let names: Vec<String> = archive
        .entries()
        .map(|entry| {
            entry
                .unwrap()
                .file_path()
                .try_normalize()
                .unwrap()
                .as_str()
                .to_string()
        })
        .collect();
    assert_eq!(names, vec!["bin/app", "bin/helper", "docs/guide.txt"]);
}
