mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use deser_value::Value;
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
use stuff::publish::{NpmPublish, Options, PublishRequest, Request, Registry, Status};
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mynpm";
const STALE_BASE: &str = "http://127.0.0.1:9/mirror/";
const TARBALL_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/tarballs/flatdemo-1.2.3.tgz");
const LOCK_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/locks/pnpm-v9-local.yaml");

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(pairs: &[(&str, String)]) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let mut saved = Vec::new();
        for (name, value) in pairs {
            saved.push((name.to_string(), std::env::var(name).ok()));
            unsafe {
                std::env::set_var(name, value);
            }
        }
        EnvGuard { _lock: lock, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => unsafe {
                    std::env::set_var(&name, value);
                },
                None => unsafe {
                    std::env::remove_var(&name);
                },
            }
        }
    }
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
    let digest = format!("{:x}", Sha512::digest(tarball));
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
    let manifest = format!("{{\"name\":\"{name}\",\"version\":\"{version}\"}}");
    let mut data = Vec::new();
    for (path, contents) in [
        ("package/package.json", manifest.as_bytes()),
        ("package/index.js", b"module.exports=1;".as_slice()),
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
        let digest = format!("{:x}", Sha512::digest(bytes));
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

fn request(lockfile: &Path) -> Request {
    Request::Publish {
        request: PublishRequest::Npm(NpmPublish {
            lockfile: lockfile.to_string_lossy().into_owned(),
            project: "demo".to_string(),
        }),
        options: Options::default(),
    }
}

fn remote_body(fake: &FakeS3, key: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == BUCKET && object.key == key)
        .map(|object| object.body)
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
    fs::write(&script, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        directory.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (path, marker)
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

    let cache = work.path().join("stuff-cache");
    let state = work.path().join("stuff-state");
    let config = work.path().join("stuff-config");
    let (path, marker) = decoy_path(work.path());
    let _env = EnvGuard::set(&[
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CACHE_HOME", cache.display().to_string()),
        ("STUFF_STATE_HOME", state.display().to_string()),
        ("STUFF_CONFIG_HOME", config.display().to_string()),
        ("PATH", path),
    ]);

    let outcome = stuff::publish::run(request(&lockfile)).unwrap();
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

    let second = stuff::publish::run(request(&lockfile)).unwrap();
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
    let cache = work.path().join("stuff-cache");
    let state = work.path().join("stuff-state");
    let config = work.path().join("stuff-config");
    let (path, marker) = decoy_path(work.path());
    let _env = EnvGuard::set(&[
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CACHE_HOME", cache.display().to_string()),
        ("STUFF_STATE_HOME", state.display().to_string()),
        ("STUFF_CONFIG_HOME", config.display().to_string()),
        ("PATH", path),
    ]);
    assert!(!state.join("npm").exists(), "the test started with local mynpm state");

    let outcome = stuff::publish::run(request(&lockfile)).unwrap();
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert!(!marker.exists(), "an mynpm child process was spawned");

    let promoted = remote_body(&fake, "current").unwrap();
    let generation = String::from_utf8(promoted).unwrap();
    let manifest =
        remote_body(&fake, &format!("generations/{generation}/manifest.json")).unwrap();
    let text = String::from_utf8(manifest).unwrap();
    assert!(text.contains("\"flatdemo\""), "{text}");
    assert!(text.contains("\"retained-demo\""), "{text}");

    let second = stuff::publish::run(request(&lockfile)).unwrap();
    assert_eq!(second.status, Status::Unchanged, "{second:?}");
}
