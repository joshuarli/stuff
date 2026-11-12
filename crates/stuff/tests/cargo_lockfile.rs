#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mynpm::acquire::{Exchange, FetchedResponse};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use stuff::publish::cargo::mirror::publish_with_exchange;
use stuff::publish::{Environment, ExitCode, Options, Status};
use support::{lower_hex, FakeS3, ACCESS_KEY, SECRET_KEY};

const INDEX: &str = "https://index.example/";
const DL: &str = "https://static.example/crates";
const BUCKET: &str = "stuff-mycrates";
const UPSTREAM_STATE: &str = "state/cargo/upstream";

fn crate_archive(name: &str, version: &str) -> Vec<u8> {
    let manifest = format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2021\"\n");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, format!("{name}-{version}/Cargo.toml"), manifest.as_bytes())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

struct Upstream {
    crates: FxHashMap<(String, String), (Vec<u8>, String)>,
    requests: Mutex<Vec<String>>,
}

impl Upstream {
    fn new(crates: &[(&str, &str)]) -> Self {
        let crates = crates
            .iter()
            .map(|(name, version)| {
                let archive = crate_archive(name, version);
                let checksum = lower_hex(Sha256::digest(&archive));
                ((name.to_string(), version.to_string()), (archive, checksum))
            })
            .collect();
        Upstream { crates, requests: Mutex::new(Vec::new()) }
    }

    fn checksum(&self, name: &str, version: &str) -> String {
        self.crates[&(name.to_string(), version.to_string())].1.clone()
    }

    fn line(&self, name: &str, version: &str) -> String {
        format!(
            "{{\"name\":\"{name}\",\"vers\":\"{version}\",\"deps\":[],\"cksum\":\"{}\",\"features\":{{}},\"yanked\":false}}",
            self.checksum(name, version)
        )
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Exchange for Upstream {
    fn fetch(&self, url: &stuff_url::Url) -> mynpm::error::Result<FetchedResponse> {
        let url = url.as_str().to_string();
        self.requests.lock().unwrap().push(url.clone());
        let ok = |body: Vec<u8>| Ok(FetchedResponse { status: 200, location: None, body });
        if url == format!("{INDEX}config.json") {
            return ok(format!("{{\"dl\":\"{DL}\"}}").into_bytes());
        }
        for ((name, version), (archive, _)) in &self.crates {
            let shard = stuff_cargo_shard::shard_for_name(&name.to_lowercase()).unwrap();
            if url == format!("{INDEX}{shard}") {
                let lines: Vec<String> = self
                    .crates
                    .keys()
                    .filter(|(other, _)| other == name)
                    .map(|(other, other_version)| self.line(other, other_version))
                    .collect();
                return ok(lines.join("\n").into_bytes());
            }
            if url == format!("{DL}/{name}/{version}/download") {
                return Ok(FetchedResponse {
                    status: 302,
                    location: Some(format!("https://cdn.example/{name}-{version}.crate")),
                    body: Vec::new(),
                });
            }
            if url == format!("https://cdn.example/{name}-{version}.crate") {
                return ok(archive.clone());
            }
        }
        Err(mynpm::error::Error::Operation(format!("upstream returned HTTP 404 for {url}")))
    }
}

fn environment(work: &Path, fake: &FakeS3) -> Environment {
    let config = work.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.json"),
        "{\"cargo\":{\"bucket\":\"stuff-mycrates\",\"public_base_url\":\"https://crates.example\"}}\n",
    )
    .unwrap();
    let mut pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| !name.starts_with("STUFF_") && !name.starts_with("R2_"))
        .collect();
    for (name, value) in [
        ("STUFF_CONFIG_HOME", config.display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("R2_S3_REGION", "auto".to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
}

fn lockfile(work: &Path, name: &str, packages: &[(&str, &str, String)]) -> PathBuf {
    let mut text = String::from("version = 4\n\n[[package]]\nname = \"demo\"\nversion = \"0.1.0\"\n");
    for (crate_name, version, checksum) in packages {
        text.push_str(&format!(
            "\n[[package]]\nname = \"{crate_name}\"\nversion = \"{version}\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n"
        ));
    }
    text.push_str("\n[[package]]\nname = \"forked\"\nversion = \"1.0.0\"\nsource = \"git+https://github.com/acme/forked#abc\"\n");
    let path = work.join(name);
    fs::write(&path, text).unwrap();
    path
}

fn remote(fake: &FakeS3, suffix: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == BUCKET && object.key.ends_with(suffix))
        .map(|object| object.body)
}

fn publish(
    locks: &[&Path],
    environment: &Environment,
    upstream: &Upstream,
    dry_run: bool,
) -> Result<stuff::publish::Outcome, stuff::publish::PublishError> {
    let locks: Vec<String> = locks.iter().map(|path| path.display().to_string()).collect();
    publish_with_exchange(&locks, Options { dry_run, ..Options::default() }, environment, upstream, INDEX)
}

#[test]
fn a_lockfile_extends_the_crates_io_view_with_verified_index_lines_and_blobs() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let environment = environment(work.path(), &fake);
    let upstream = Upstream::new(&[("serde", "1.0.0"), ("rustybench", "0.5.0")]);
    let lock = lockfile(
        work.path(),
        "Cargo.lock",
        &[
            ("serde", "1.0.0", upstream.checksum("serde", "1.0.0")),
            ("rustybench", "0.5.0", upstream.checksum("rustybench", "0.5.0")),
        ],
    );

    let outcome = publish(&[&lock], &environment, &upstream, false).unwrap();
    assert_eq!(outcome.status, Status::Published);
    assert_eq!(outcome.packages, vec!["rustybench@0.5.0", "serde@1.0.0"]);
    let index = remote(&fake, "/crates-io/se/rd/serde").expect("serde index file");
    assert_eq!(String::from_utf8(index).unwrap().trim(), upstream.line("serde", "1.0.0"));
    let checksum = upstream.checksum("rustybench", "0.5.0");
    assert_eq!(
        remote(&fake, &format!("{checksum}.crate")).expect("rustybench blob"),
        upstream.crates[&("rustybench".to_string(), "0.5.0".to_string())].0
    );

    assert!(work.path().join(UPSTREAM_STATE).join("serde").is_dir(), "the publish kept no canonical state");
    let before = upstream.requests().len();
    let rerun = publish(&[&lock], &environment, &upstream, false).unwrap();
    assert_eq!(rerun.status, Status::Unchanged);
    assert!(rerun.packages.is_empty());
    assert_eq!(upstream.requests().len(), before, "a present crate was fetched again");
}

#[test]
fn only_crates_missing_from_the_live_generation_are_downloaded() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let environment = environment(work.path(), &fake);
    let upstream = Upstream::new(&[("serde", "1.0.0"), ("rustybench", "0.5.0")]);
    let first = lockfile(work.path(), "first.lock", &[("serde", "1.0.0", upstream.checksum("serde", "1.0.0"))]);
    publish(&[&first], &environment, &upstream, false).unwrap();
    fs::remove_dir_all(work.path().join("state")).unwrap();

    let both = lockfile(
        work.path(),
        "both.lock",
        &[
            ("serde", "1.0.0", upstream.checksum("serde", "1.0.0")),
            ("rustybench", "0.5.0", upstream.checksum("rustybench", "0.5.0")),
        ],
    );
    let before = upstream.requests().len();
    let outcome = publish(&[&both], &environment, &upstream, false).unwrap();
    assert_eq!(outcome.packages, vec!["rustybench@0.5.0"]);
    assert!(
        upstream.requests()[before..].iter().all(|url| !url.contains("serde")),
        "serde is live but was fetched: {:?}",
        &upstream.requests()[before..]
    );
    assert!(remote(&fake, "/crates-io/se/rd/serde").is_some(), "the live serde line was dropped");
}

#[test]
fn checksum_disagreements_fail_with_integrity_and_promote_nothing() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let environment = environment(work.path(), &fake);
    let upstream = Upstream::new(&[("serde", "1.0.0")]);
    let wrong = lockfile(work.path(), "Cargo.lock", &[("serde", "1.0.0", "0".repeat(64))]);

    let error = publish(&[&wrong], &environment, &upstream, false).unwrap_err();
    assert_eq!(error.classification(), ExitCode::Integrity, "{error}");
    assert!(error.to_string().contains("trust chain violation"), "{error}");
    assert!(remote(&fake, "current").is_none(), "a generation was promoted");
}

#[test]
fn a_dry_run_stages_without_touching_the_remote_or_the_state() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let environment = environment(work.path(), &fake);
    let upstream = Upstream::new(&[("serde", "1.0.0")]);
    let lock = lockfile(work.path(), "Cargo.lock", &[("serde", "1.0.0", upstream.checksum("serde", "1.0.0"))]);

    let outcome = publish(&[&lock], &environment, &upstream, true).unwrap();
    assert!(outcome.dry_run);
    assert_eq!(outcome.packages, vec!["serde@1.0.0"]);
    assert!(fake.state().is_empty(), "the dry run wrote to the remote");
    assert!(!work.path().join(UPSTREAM_STATE).join("serde").exists(), "the dry run wrote canonical state");
}
