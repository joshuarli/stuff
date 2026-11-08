#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use deser_value::Value;
use mynpm::acquire::{Exchange, FetchedResponse};
use mynpm::inventory::{
    validate_artifact_record, ArtifactRecord, Inventory, LockfileVersion, Manager, PackageFlags,
    PackageRecord, PlatformConstraints, ProjectRecord, SourceKind,
};
use mynpm::inventory_store::save_inventory;
use mynpm::publish::{publish_site, PublishOptions};
use sha2::{Digest, Sha512};
use stuff::publish::config::Config;
use stuff::publish::inventory;
use stuff::publish::npm;
use stuff::publish::r2::{Credentials, R2Client};
use stuff::publish::snapshot::Snapshot;
use stuff::publish::{Environment, NpmPublish, Options, PublishRequest, Request, Registry, Status};
use support::{write_executable, lower_hex, FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mynpm";
const STALE_BASE: &str = "http://127.0.0.1:9/mirror/";
const TARBALL_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/tarballs/flatdemo-1.2.3.tgz");
const LOCK_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/locks/pnpm-v9-local.yaml");

fn environment(work: &Path, fake: &FakeS3, path: &str) -> Environment {
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
        ("PATH", path.to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
}

fn credentials(fake: &FakeS3) -> Credentials {
    Credentials {
        endpoint: fake.base_url().to_string(),
        access_key_id: ACCESS_KEY.to_string(),
        secret_access_key: SECRET_KEY.to_string(),
        region: "auto".to_string(),
        session_token: None,
    }
}

fn artifact(name: &str, version: &str, tarball: &[u8]) -> ArtifactRecord {
    let digest = lower_hex(Sha512::digest(tarball));
    let raw: Vec<u8> = (0..digest.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&digest[index..index + 2], 16).unwrap())
        .collect();
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(&raw)
    );
    let source = format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz");
    let text = format!(
        "{{\"name\":\"{name}\",\"version\":\"{version}\",\"packument\":{{\"name\":\"{name}\",\"dist-tags\":{{\"latest\":\"{version}\"}}}},\"metadata\":{{\"name\":\"{name}\",\"version\":\"{version}\",\"dist\":{{\"tarball\":\"{source}\"}}}},\"artifact\":{{\"sha512\":\"{digest}\",\"integrity\":\"{integrity}\",\"shasum\":null,\"sourceUrl\":\"{source}\",\"legacySha1Only\":false}}}}"
    );
    let value: Value = deser_json::from_str(&text).unwrap();
    validate_artifact_record(&value).unwrap()
}

fn tarball(name: &str, version: &str) -> Vec<u8> {
    tarball_with(name, version, b"module.exports=1;")
}

fn tarball_with(name: &str, version: &str, index: &[u8]) -> Vec<u8> {
    let manifest = format!(
        "{{\"name\":\"{name}\",\"version\":\"{version}\",\"description\":\"first party\",\"type\":\"module\",\"peerDependencies\":{{\"solid-js\":\">=2.0.0\"}},\"scripts\":{{\"build\":\"never copied\"}}}}"
    );
    let mut data = Vec::new();
    for (path, contents) in [
        ("package/package.json", manifest.as_bytes()),
        ("package/index.js", index),
    ] {
        let mut header = vec![0u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..262].copy_from_slice(b"ustar");
        header[263..265].copy_from_slice(b"00");
        header[124..132].copy_from_slice(format!("{:07o}\0", contents.len()).as_bytes());
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        data.extend_from_slice(&header);
        data.extend_from_slice(contents);
        while data.len() % 512 != 0 {
            data.push(0);
        }
    }
    data.extend_from_slice(&[0u8; 1024]);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&data).unwrap();
    encoder.finish().unwrap()
}

fn package(record: &ArtifactRecord) -> PackageRecord {
    PackageRecord {
        key: format!("{}@{}", record.name, record.version),
        name: record.name.clone(),
        version: Some(record.version.clone()),
        integrity: Some(record.artifact.integrity.clone()),
        resolved: Some(record.artifact.source_url.clone()),
        source_kind: SourceKind::Registry,
        aliases: Vec::new(),
        flags: PackageFlags {
            dev: false,
            optional: false,
            peer: false,
            peer_optional: false,
            link: false,
        },
        platform: PlatformConstraints { os: None, cpu: None, libc: None },
        targets: vec!["mac-arm64".to_string()],
    }
}

fn build_live(work: &Path, packages: &[(&str, &str, Vec<u8>)]) -> (PathBuf, String) {
    let state = work.join("live-state");
    let site = work.join("live-site");
    let artifacts: Vec<ArtifactRecord> = packages
        .iter()
        .map(|(name, version, bytes)| artifact(name, version, bytes))
        .collect();
    let project = ProjectRecord {
        id: "live".to_string(),
        manager: Manager::Pnpm,
        package_manager_pin: None,
        lockfile_path: "pnpm-lock.yaml".to_string(),
        lockfile_version: LockfileVersion::Pnpm("9.0".to_string()),
        targets: vec!["mac-arm64".to_string()],
        importers: vec![".".to_string()],
        roots: Vec::new(),
        packages: artifacts.iter().map(package).collect(),
        snapshot: None,
    };
    let inventory = Inventory {
        projects: vec![project],
        artifacts,
        metadata_only: Vec::new(),
    };
    save_inventory(&state, &inventory).unwrap();
    let objects = state.join("objects").join("sha512");
    fs::create_dir_all(&objects).unwrap();
    for (_, _, bytes) in packages {
        let digest = lower_hex(Sha512::digest(bytes));
        fs::write(objects.join(format!("{digest}.tgz")), bytes).unwrap();
    }
    let summary = publish_site(&PublishOptions {
        state_directory: &state,
        site_directory: &site,
        config: &npm::library_config(&Config::default()),
        render_base_url: Some(STALE_BASE.to_string()),
    })
    .unwrap();
    (site, summary.generation_id)
}

fn is_alias(relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    parts.len() >= 3
        && parts[0] == "generations"
        && (parts[2] == "tarballs" || (parts[2] == "-" && parts.get(3) == Some(&"tarballs")))
}

fn seed_bucket(fake: &FakeS3, live_site: &Path, generation: &str) {
    let mirror = live_site.join("mirror");
    let mut stack = vec![mirror.clone()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let file_type = entry.file_type().unwrap();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(&mirror)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative.starts_with('.') || is_alias(&relative) {
                continue;
            }
            let body = fs::read(&path).unwrap();
            fake.seed(BUCKET, &relative, &body, "application/octet-stream");
        }
    }
    fake.seed(BUCKET, "current", generation.as_bytes(), "text/plain");
}

fn direct_generation(work: &Path, fake: &FakeS3, lockfile: &Path) -> String {
    let exchange = UnreachableExchange;
    let site = work.join("direct-site");
    let state = work.join("direct-state");
    let cache = work.join("direct-cache");
    let config = Config::default();
    let library = npm::library_config(&config);
    let snapshot =
        Snapshot::open(R2Client::new(credentials(fake)), Registry::Npm, &config, cache).unwrap();
    let store = snapshot.shared_stores().first().cloned().unwrap();
    inventory::prepare(
        &snapshot,
        &inventory::PrepareOptions {
            bucket: BUCKET,
            store: &store,
            site_dir: &site,
            state_dir: &state,
            lockfile,
            project_id: "demo",
            library: &library,
            exchange: &exchange,
        },
    )
    .unwrap();
    let summary = publish_site(&PublishOptions {
        state_directory: &state,
        site_directory: &site,
        config: &library,
        render_base_url: None,
    })
    .unwrap();
    summary.generation_id
}

fn request(lockfile: &Path, dry_run: bool) -> Request {
    Request::Publish {
        request: PublishRequest::Npm(NpmPublish {
            lockfile: lockfile.to_string_lossy().into_owned(),
            project: "demo".to_string(),
        }),
        options: Options {
            dry_run,
            ..Options::default()
        },
    }
}

fn remote_body(fake: &FakeS3, key: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == BUCKET && object.key == key)
        .map(|object| object.body)
}

fn tree(root: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut stack = if root.exists() {
        vec![root.to_path_buf()]
    } else {
        Vec::new()
    };
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn remote_snapshot(fake: &FakeS3) -> Vec<(String, String, u64, String, Vec<u8>)> {
    let mut objects: Vec<_> = fake
        .state()
        .into_iter()
        .map(|object| (object.bucket, object.key, object.size, object.etag, object.body))
        .collect();
    objects.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    objects
}

fn mutation_count(fake: &FakeS3, after: usize) -> usize {
    fake.requests()[after..]
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count()
}

fn compare_generation(fake: &FakeS3, generation: &str, direct_site: &Path) {
    let mirror = direct_site.join("mirror").join("generations").join(generation);
    let prefix = format!("generations/{generation}/");
    let mut checked = 0;
    for object in fake.state() {
        if object.bucket != BUCKET || !object.key.starts_with(&prefix) {
            continue;
        }
        let relative = object.key.strip_prefix(&prefix).unwrap();
        let expected = fs::read(mirror.join(relative))
            .unwrap_or_else(|error| panic!("missing local {relative}: {error}"));
        assert_eq!(object.body, expected, "{relative}");
        checked += 1;
    }
    assert!(checked > 0, "no generation objects were uploaded");
}

fn decoy_path(work: &Path) -> (String, PathBuf) {
    let directory = work.join("bin");
    fs::create_dir(&directory).unwrap();
    let marker = work.join("mynpm-was-run");
    let script = directory.join("mynpm");
    write_executable(
        &script,
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    );
    let path = format!(
        "{}:{}",
        directory.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (path, marker)
}

const NETWORK_NAME: &str = "network-demo";
const NETWORK_VERSION: &str = "2.0.0";
const NETWORK_TARBALL: &str =
    "https://registry.npmjs.org/network-demo/-/network-demo-2.0.0.tgz";
const NETWORK_PACKUMENT: &str = "https://registry.npmjs.org/network-demo";

fn integrity(bytes: &[u8]) -> String {
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
    )
}

struct UnreachableExchange;

impl Exchange for UnreachableExchange {
    fn fetch(&self, url: &stuff_url::Url) -> mynpm::error::Result<FetchedResponse> {
        Err(mynpm::error::Error::Operation(format!(
            "unexpected upstream request {url}"
        )))
    }
}

struct RecordingExchange {
    tarball: Vec<u8>,
    requests: Mutex<Vec<String>>,
}

impl RecordingExchange {
    fn new(tarball: Vec<u8>) -> Self {
        RecordingExchange {
            tarball,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Exchange for RecordingExchange {
    fn fetch(&self, url: &stuff_url::Url) -> mynpm::error::Result<FetchedResponse> {
        self.requests.lock().unwrap().push(url.as_str().to_string());
        match url.as_str() {
            NETWORK_TARBALL => Ok(FetchedResponse {
                status: 200,
                location: None,
                body: self.tarball.clone(),
            }),
            NETWORK_PACKUMENT => {
                let integrity = integrity(&self.tarball);
                let body = format!(
                    "{{\"name\":\"{NETWORK_NAME}\",\"dist-tags\":{{\"latest\":\"{NETWORK_VERSION}\"}},\"versions\":{{\"{NETWORK_VERSION}\":{{\"name\":\"{NETWORK_NAME}\",\"version\":\"{NETWORK_VERSION}\",\"dist\":{{\"tarball\":\"{NETWORK_TARBALL}\",\"integrity\":\"{integrity}\"}}}}}}}}"
                );
                Ok(FetchedResponse {
                    status: 200,
                    location: None,
                    body: body.into_bytes(),
                })
            }
            other => Err(mynpm::error::Error::Operation(format!(
                "unexpected upstream request {other}"
            ))),
        }
    }
}

fn network_lockfile(work: &Path, integrity: &str) -> PathBuf {
    let path = work.join("package-lock.json");
    let text = format!(
        "{{\"name\":\"demo\",\"version\":\"1.0.0\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{{\"\":{{\"name\":\"demo\",\"version\":\"1.0.0\",\"dependencies\":{{\"{NETWORK_NAME}\":\"{NETWORK_VERSION}\"}}}},\"node_modules/{NETWORK_NAME}\":{{\"version\":\"{NETWORK_VERSION}\",\"resolved\":\"{NETWORK_TARBALL}\",\"integrity\":\"{integrity}\"}}}}}}"
    );
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn a_lockfile_package_absent_from_live_and_state_is_downloaded_and_published() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let (live_site, live_generation) =
        build_live(work.path(), &[("retained-demo", "1.0.0", tarball("retained-demo", "1.0.0"))]);
    seed_bucket(&fake, &live_site, &live_generation);

    let tarball_bytes = tarball(NETWORK_NAME, NETWORK_VERSION);
    let lockfile = network_lockfile(work.path(), &integrity(&tarball_bytes));
    let exchange = RecordingExchange::new(tarball_bytes.clone());
    assert!(!work.path().join("stuff-state/npm").exists(), "the test started with npm state");

    let (path, marker) = decoy_path(work.path());
    let environment = environment(work.path(), &fake, &path);
    let outcome = stuff::publish::npm::publish_with_exchange(
        &NpmPublish {
            lockfile: lockfile.to_string_lossy().into_owned(),
            project: "demo".to_string(),
        },
        Options::default(),
        &environment,
        &exchange,
    )
    .unwrap();
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert!(!marker.exists(), "an mynpm child process was spawned");

    let requests = exchange.requests();
    assert!(
        requests.iter().any(|url| url == NETWORK_TARBALL),
        "the tarball was never requested: {requests:?}"
    );
    assert!(
        requests.iter().any(|url| url == NETWORK_PACKUMENT),
        "the packument was never requested: {requests:?}"
    );

    let digest = lower_hex(Sha512::digest(&tarball_bytes));
    assert!(
        work.path()
            .join("stuff-state/npm/objects/sha512")
            .join(format!("{digest}.tgz"))
            .is_file(),
        "the acquired tarball was not stored in state"
    );

    let generation = String::from_utf8(remote_body(&fake, "current").unwrap()).unwrap();
    let manifest =
        remote_body(&fake, &format!("generations/{generation}/manifest.json")).unwrap();
    let text = String::from_utf8(manifest).unwrap();
    assert!(text.contains(NETWORK_NAME), "{text}");
    assert!(text.contains("retained-demo"), "{text}");
}

#[test]
fn publish_matches_the_library_and_retains_the_live_packages() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let (live_site, live_generation) = build_live(
        work.path(),
        &[
            ("flatdemo", "1.2.3", fs::read(TARBALL_FIXTURE).unwrap()),
            ("retained-demo", "1.0.0", tarball("retained-demo", "1.0.0")),
        ],
    );
    seed_bucket(&fake, &live_site, &live_generation);

    let lockfile = PathBuf::from(LOCK_FIXTURE);
    let direct = direct_generation(work.path(), &fake, &lockfile);
    assert_ne!(direct, live_generation);

    let (path, marker) = decoy_path(work.path());
    let environment = environment(work.path(), &fake, &path);

    let outcome = stuff::publish::run(request(&lockfile, false), &environment).unwrap();
    assert_eq!(outcome.registry, Registry::Npm);
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert!(!marker.exists(), "an mynpm child process was spawned");

    assert_eq!(
        remote_body(&fake, "current").unwrap(),
        direct.as_bytes(),
        "the promoted generation id differs from the library publish"
    );
    compare_generation(&fake, &direct, &work.path().join("direct-site"));

    let manifest = remote_body(&fake, &format!("generations/{direct}/manifest.json")).unwrap();
    let text = String::from_utf8(manifest).unwrap();
    assert!(text.contains("\"flatdemo\""), "{text}");
    assert!(text.contains("\"retained-demo\""), "{text}");

    let second = stuff::publish::run(request(&lockfile, false), &environment).unwrap();
    assert_eq!(second.status, Status::Unchanged, "{second:?}");
    assert!(!marker.exists(), "an mynpm child process was spawned");
    assert_eq!(
        remote_body(&fake, "current").unwrap(),
        direct.as_bytes(),
        "the unchanged publish moved the current generation"
    );
}

#[test]
fn publish_without_local_state_keeps_the_live_packages() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let (live_site, live_generation) = build_live(
        work.path(),
        &[
            ("flatdemo", "1.2.3", fs::read(TARBALL_FIXTURE).unwrap()),
            ("retained-demo", "1.0.0", tarball("retained-demo", "1.0.0")),
        ],
    );
    seed_bucket(&fake, &live_site, &live_generation);

    let lockfile = PathBuf::from(LOCK_FIXTURE);
    let state = work.path().join("stuff-state");
    let (path, marker) = decoy_path(work.path());
    let environment = environment(work.path(), &fake, &path);
    assert!(!state.join("npm").exists(), "the test started with local mynpm state");

    let outcome = stuff::publish::run(request(&lockfile, false), &environment).unwrap();
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert!(!marker.exists(), "an mynpm child process was spawned");

    let promoted = remote_body(&fake, "current").unwrap();
    let generation = String::from_utf8(promoted).unwrap();
    let manifest =
        remote_body(&fake, &format!("generations/{generation}/manifest.json")).unwrap();
    let text = String::from_utf8(manifest).unwrap();
    assert!(text.contains("\"flatdemo\""), "{text}");
    assert!(text.contains("\"retained-demo\""), "{text}");

    let second = stuff::publish::run(request(&lockfile, false), &environment).unwrap();
    assert_eq!(second.status, Status::Unchanged, "{second:?}");
}

#[test]
fn an_npm_dry_run_leaves_local_state_and_remote_unchanged() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let (live_site, live_generation) = build_live(
        work.path(),
        &[
            ("flatdemo", "1.2.3", fs::read(TARBALL_FIXTURE).unwrap()),
            ("retained-demo", "1.0.0", tarball("retained-demo", "1.0.0")),
        ],
    );
    seed_bucket(&fake, &live_site, &live_generation);

    let lockfile = PathBuf::from(LOCK_FIXTURE);
    let (path, marker) = decoy_path(work.path());
    let environment = environment(work.path(), &fake, &path);
    let first = stuff::publish::run(request(&lockfile, false), &environment).unwrap();
    assert_eq!(first.status, Status::Published, "{first:?}");
    assert!(!marker.exists(), "an mynpm child process was spawned");

    let state = work.path().join("stuff-state");
    let site = work.path().join("stuff-cache/npm/site");
    let state_before = tree(&state);
    let site_before = tree(&site);
    let remote_before = remote_snapshot(&fake);
    let requests_before = fake.requests().len();
    let conditions_before = fake.put_conditions().len();
    assert!(!state_before.is_empty(), "the first publish wrote no local state");
    assert!(!site_before.is_empty(), "the first publish wrote no local site");

    let dry = stuff::publish::run(request(&lockfile, true), &environment).unwrap();
    assert_eq!(dry.registry, Registry::Npm);

    assert_eq!(tree(&state), state_before, "the dry run changed local state");
    assert_eq!(tree(&site), site_before, "the dry run changed the local site");
    assert_eq!(remote_snapshot(&fake), remote_before, "the dry run changed the remote");
    assert_eq!(fake.put_conditions().len(), conditions_before);
    assert_eq!(mutation_count(&fake, requests_before), 0, "the dry run mutated the remote");
    assert!(!marker.exists(), "an mynpm child process was spawned");
}

const FIRST_PARTY: &str = "@scope/ui";

fn pack(work: &Path, label: &str, name: &str, version: &str, index: &[u8]) -> (String, Vec<u8>) {
    let bytes = tarball_with(name, version, index);
    let path = work.join(format!("{label}.tgz"));
    fs::write(&path, &bytes).unwrap();
    (path.to_string_lossy().into_owned(), bytes)
}

fn tarball_request(paths: &[&str], dry_run: bool) -> Request {
    Request::Publish {
        request: PublishRequest::NpmTarballs(paths.iter().map(|path| path.to_string()).collect()),
        options: Options {
            dry_run,
            ..Options::default()
        },
    }
}

fn current_file(fake: &FakeS3, relative: &str) -> Option<Vec<u8>> {
    let generation = String::from_utf8(remote_body(fake, "current")?).unwrap();
    remote_body(fake, &format!("generations/{generation}/{relative}"))
}

fn packument(fake: &FakeS3, name: &str) -> Value {
    let body = current_file(fake, name).unwrap_or_else(|| panic!("no packument for {name}"));
    deser_json::from_str(std::str::from_utf8(&body).unwrap()).unwrap()
}

fn live_with_retained(work: &Path, fake: &FakeS3) -> String {
    let (live_site, live_generation) = build_live(
        work,
        &[
            ("flatdemo", "1.2.3", fs::read(TARBALL_FIXTURE).unwrap()),
            ("retained-demo", "1.0.0", tarball("retained-demo", "1.0.0")),
        ],
    );
    seed_bucket(fake, &live_site, &live_generation);
    live_generation
}

#[test]
fn a_first_party_scoped_package_is_hosted_beside_the_mirror() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (path, bytes) = pack(work.path(), "ui-1", FIRST_PARTY, "0.1.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());

    let outcome = stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap();
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert_eq!(outcome.packages, vec!["@scope/ui@0.1.0".to_string()]);

    let document = packument(&fake, FIRST_PARTY);
    let map = document.as_map().unwrap();
    assert_eq!(map.get("name").and_then(|v| v.as_str()), Some(FIRST_PARTY));
    let tags = map.get("dist-tags").and_then(|v| v.as_map()).unwrap();
    assert_eq!(tags.get("latest").and_then(|v| v.as_str()), Some("0.1.0"));
    let version = map.get("versions").and_then(|v| v.as_map()).unwrap().get("0.1.0").unwrap();
    let version = version.as_map().unwrap();
    assert_eq!(version.get("description").and_then(|v| v.as_str()), Some("first party"));
    assert!(version.get("scripts").is_none(), "scripts must not reach the packument");
    assert!(version.get("peerDependencies").is_some());
    let dist = version.get("dist").and_then(|v| v.as_map()).unwrap();
    assert_eq!(dist.get("integrity").and_then(|v| v.as_str()), Some(integrity(&bytes).as_str()));
    let digest = lower_hex(Sha512::digest(&bytes));
    assert_eq!(
        dist.get("tarball").and_then(|v| v.as_str()),
        Some(format!("https://npm.17166969.xyz/current/-/tarballs/{digest}.tgz").as_str())
    );
    assert!(
        fake.state().iter().any(|object| object.bucket == BUCKET && object.body == bytes),
        "the tarball was not uploaded"
    );
    for retained in ["flatdemo", "retained-demo"] {
        assert!(current_file(&fake, retained).is_some(), "{retained} was dropped");
    }
}

#[test]
fn republishing_identical_bytes_changes_nothing_and_new_versions_append() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (first, _) = pack(work.path(), "ui-1", FIRST_PARTY, "0.1.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());
    stuff::publish::run(tarball_request(&[&first], false), &environment).unwrap();
    let promoted = remote_body(&fake, "current").unwrap();
    let before = remote_snapshot(&fake);
    let requests_before = fake.requests().len();

    let again = stuff::publish::run(tarball_request(&[&first], false), &environment).unwrap();
    assert_eq!(again.status, Status::Unchanged, "{again:?}");
    assert!(again.packages.is_empty(), "{again:?}");
    assert_eq!(remote_body(&fake, "current").unwrap(), promoted);
    assert_eq!(remote_snapshot(&fake), before);
    assert_eq!(mutation_count(&fake, requests_before), 0);

    let (second, _) = pack(work.path(), "ui-2", FIRST_PARTY, "0.2.0", b"export const two = 2;");
    let appended = stuff::publish::run(tarball_request(&[&second], false), &environment).unwrap();
    assert_eq!(appended.status, Status::Published, "{appended:?}");
    let document = packument(&fake, FIRST_PARTY);
    let map = document.as_map().unwrap();
    let versions = map.get("versions").and_then(|v| v.as_map()).unwrap();
    assert!(versions.get("0.1.0").is_some() && versions.get("0.2.0").is_some());
    let tags = map.get("dist-tags").and_then(|v| v.as_map()).unwrap();
    assert_eq!(tags.get("latest").and_then(|v| v.as_str()), Some("0.2.0"));
    let time = map.get("time").and_then(|v| v.as_map()).unwrap();
    assert!(time.get("0.1.0").is_some() && time.get("0.2.0").is_some());
}

#[test]
fn different_bytes_for_a_published_first_party_version_conflict() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (first, _) = pack(work.path(), "ui-1", FIRST_PARTY, "0.1.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());
    stuff::publish::run(tarball_request(&[&first], false), &environment).unwrap();
    let before = remote_snapshot(&fake);
    let requests_before = fake.requests().len();

    let (changed, _) = pack(work.path(), "ui-changed", FIRST_PARTY, "0.1.0", b"export const x = 1;");
    let error = stuff::publish::run(tarball_request(&[&changed], false), &environment).unwrap_err();
    assert_eq!(error.exit_code(), 3, "{error}");
    assert_eq!(remote_snapshot(&fake), before);
    assert_eq!(mutation_count(&fake, requests_before), 0);
}

#[test]
fn a_first_party_name_cannot_reuse_a_mirrored_name() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (path, _) = pack(work.path(), "clash", "retained-demo", "9.0.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());
    let before = remote_snapshot(&fake);

    let error = stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap_err();
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(error.to_string().contains("mirror"), "{error}");
    assert_eq!(remote_snapshot(&fake), before);
}

#[test]
fn first_party_packages_survive_a_later_mirror_publish() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (path, _) = pack(work.path(), "ui-1", FIRST_PARTY, "0.1.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());
    stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap();

    let mirrored = stuff::publish::run(request(&PathBuf::from(LOCK_FIXTURE), false), &environment).unwrap();
    assert_ne!(mirrored.status, Status::Conflict, "{mirrored:?}");
    let document = packument(&fake, FIRST_PARTY);
    let versions = document.as_map().unwrap().get("versions").and_then(|v| v.as_map()).unwrap();
    assert!(versions.get("0.1.0").is_some());
    for retained in ["flatdemo", "retained-demo"] {
        assert!(current_file(&fake, retained).is_some(), "{retained} was dropped");
    }
    let generation = String::from_utf8(remote_body(&fake, "current").unwrap()).unwrap();
    let manifest = remote_body(&fake, &format!("generations/{generation}/manifest.json")).unwrap();
    assert!(String::from_utf8(manifest).unwrap().contains("\"firstParty\": true"));

    let again = stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap();
    assert_eq!(again.status, Status::Unchanged, "{again:?}");

    stuff::publish::run(
        Request::Republish { registry: Registry::Npm, allow_drops: false, options: Options::default() },
        &environment,
    )
    .unwrap();
    assert!(packument(&fake, FIRST_PARTY).as_map().is_some());
}

#[test]
fn a_lockfile_cannot_mirror_another_version_of_a_first_party_name() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (path, _) = pack(work.path(), "network", NETWORK_NAME, "1.0.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());
    stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap();

    let upstream = tarball(NETWORK_NAME, NETWORK_VERSION);
    let lockfile = network_lockfile(work.path(), &integrity(&upstream));
    let exchange = RecordingExchange::new(upstream);
    let error = stuff::publish::npm::publish_with_exchange(
        &NpmPublish {
            lockfile: lockfile.to_string_lossy().into_owned(),
            project: "demo".to_string(),
        },
        Options::default(),
        &environment,
        &exchange,
    )
    .unwrap_err();
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(exchange.requests().is_empty(), "{:?}", exchange.requests());
}

#[test]
fn a_first_party_dry_run_plans_without_touching_state_or_remote() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    live_with_retained(work.path(), &fake);
    let (path, _) = pack(work.path(), "ui-1", FIRST_PARTY, "0.1.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());
    let state = work.path().join("stuff-state");
    let before = remote_snapshot(&fake);
    let requests_before = fake.requests().len();

    let dry = stuff::publish::run(tarball_request(&[&path], true), &environment).unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.packages, vec!["@scope/ui@0.1.0".to_string()]);
    assert_eq!(remote_snapshot(&fake), before);
    assert_eq!(mutation_count(&fake, requests_before), 0);
    assert!(tree(&state).is_empty(), "the dry run wrote local state");
}

#[test]
fn a_first_party_tarball_publishes_into_an_empty_bucket() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let (path, _) = pack(work.path(), "ui-boot", FIRST_PARTY, "0.1.0", b"export {};");
    let environment = environment(work.path(), &fake, &std::env::var("PATH").unwrap());

    let published = stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap();
    assert_eq!(published.packages, vec!["@scope/ui@0.1.0".to_string()]);
    assert!(!published.generation.is_empty());
    assert!(published.uploaded_objects > 0);

    let again = stuff::publish::run(tarball_request(&[&path], false), &environment).unwrap();
    assert!(again.packages.is_empty());
    assert_eq!(again.uploaded_objects, 0);
}
