#![forbid(unsafe_code)]

use base64::Engine;
use mynpm::acquire::{fetch_inventory, verify_inventory, Exchange, FetchedResponse};
use mynpm::config::{Config, DarwinTarget, Policy, Target};
use mynpm::error::{Error, Result};
use mynpm::inventory::{
    Inventory, LockfileVersion, Manager, PackageFlags, PackageRecord, PlatformConstraints, ProjectRecord, SourceKind,
};
use mynpm::inventory_store::{load_inventory, update_inventory};
use mynpm::publish::{publish_site, PublishOptions};
use sha2::{Digest, Sha512};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

const PUBLIC_BASE: &str = "http://127.0.0.1:8080/";
const STUB_PACKUMENT_URL: &str = "https://registry.npmjs.org/@scope%2fnative";
const HOSTED_TARBALL_URL: &str = "https://registry.npmjs.org/flatdemo/-/flatdemo-1.2.3.tgz";

fn integrity_of(bytes: &[u8]) -> String {
    format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes)))
}

fn fixture_tarball() -> Vec<u8> {
    let manifest = br#"{"name":"flatdemo","version":"1.2.3"}"#;
    let mut data = Vec::new();
    for (name, contents) in [("package/package.json", &manifest[..]), ("package/index.js", b"module.exports=1;" as &[u8])] {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..262].copy_from_slice(b"ustar");
        header[263..265].copy_from_slice(b"00");
        header[124..132].copy_from_slice(format!("{:07o}\0", contents.len()).as_bytes());
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
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

struct Upstream {
    tarball: Vec<u8>,
    stub_integrity_upstream: String,
    requests: Mutex<Vec<String>>,
}

impl Upstream {
    fn requested(&self, url: &str) -> usize {
        self.requests.lock().unwrap().iter().filter(|u| *u == url).count()
    }
}

impl Exchange for Upstream {
    fn fetch(&self, url: &url::Url) -> Result<FetchedResponse> {
        self.requests.lock().unwrap().push(url.to_string());
        let body = if url.as_str() == HOSTED_TARBALL_URL {
            self.tarball.clone()
        } else if url.as_str() == STUB_PACKUMENT_URL {
            format!(
                r#"{{"name":"@scope/native","time":{{"created":"2024-01-01T00:00:00.000Z","1.0.0":"2024-01-02T03:04:05.678Z","1.1.0":"2024-02-03T04:05:06.789Z"}},"versions":{{"1.0.0":{{"name":"@scope/native","version":"1.0.0","dist":{{"integrity":"{i}","tarball":"https://registry.npmjs.org/@scope/native/-/native-1.0.0.tgz"}}}},"1.1.0":{{"name":"@scope/native","version":"1.1.0","dist":{{"integrity":"{i}","tarball":"https://registry.npmjs.org/@scope/native/-/native-1.1.0.tgz"}}}}}}}}"#,
                i = self.stub_integrity_upstream
            )
            .into_bytes()
        } else if url.as_str() == "https://registry.npmjs.org/flatdemo" {
            format!(
                r#"{{"name":"flatdemo","dist-tags":{{"latest":"1.2.3"}},"time":{{"modified":"2024-03-01T00:00:00.000Z","1.2.3":"2024-03-01T00:00:00.000Z"}},"versions":{{"1.2.3":{{"name":"flatdemo","version":"1.2.3","dist":{{"integrity":"{}","tarball":"{HOSTED_TARBALL_URL}"}}}}}}}}"#,
                integrity_of(&self.tarball)
            )
            .into_bytes()
        } else {
            return Err(Error::Operation(format!("unexpected upstream request {url}")));
        };
        Ok(FetchedResponse { status: 200, location: None, body })
    }
}

fn package(name: &str, version: &str, integrity: &str, platform: PlatformConstraints) -> PackageRecord {
    PackageRecord {
        key: format!("{name}@{version}"),
        name: name.to_string(),
        version: Some(version.to_string()),
        integrity: Some(integrity.to_string()),
        resolved: None,
        source_kind: SourceKind::Registry,
        aliases: vec![],
        flags: PackageFlags { dev: false, optional: true, peer: false, peer_optional: false, link: false },
        platform,
        targets: vec!["darwin-arm64".to_string()],
    }
}

fn no_platform() -> PlatformConstraints {
    PlatformConstraints { os: None, cpu: None, libc: None }
}

fn win32_x64() -> PlatformConstraints {
    PlatformConstraints { os: Some(vec!["win32".to_string()]), cpu: Some(vec!["x64".to_string()]), libc: None }
}

fn config() -> Config {
    Config {
        public_base_url: PUBLIC_BASE.to_string(),
        upstream_registry: "https://registry.npmjs.org/".to_string(),
        targets: vec![Target::Darwin(DarwinTarget {
            id: "darwin-arm64".to_string(),
            cpu: "arm64".to_string(),
            node: "26.12.0".to_string(),
        })],
        policy: Policy { allow_legacy_sha1_only: false },
    }
}

fn seed(state: &Path, tarball: &[u8], stub_integrity: &str) {
    seed_with_flatdemo_integrity(state, &integrity_of(tarball), stub_integrity);
}

fn seed_with_flatdemo_integrity(state: &Path, flatdemo_integrity: &str, stub_integrity: &str) {
    let project = ProjectRecord {
        id: "demo".to_string(),
        manager: Manager::Pnpm,
        package_manager_pin: None,
        lockfile_path: "pnpm-lock.yaml".to_string(),
        lockfile_version: LockfileVersion::Pnpm("9.0".to_string()),
        targets: vec!["darwin-arm64".to_string()],
        importers: vec![".".to_string()],
        roots: vec![],
        packages: vec![
            package("flatdemo", "1.2.3", flatdemo_integrity, no_platform()),
            package("@scope/native", "1.0.0", stub_integrity, win32_x64()),
            package("@scope/native", "1.1.0", stub_integrity, win32_x64()),
        ],
        snapshot: None,
    };
    update_inventory(state, |_| Ok(Inventory { projects: vec![project], artifacts: vec![], metadata_only: vec![] })).unwrap();
}

fn upstream(tarball: Vec<u8>, stub_integrity: &str) -> Upstream {
    Upstream { tarball, stub_integrity_upstream: stub_integrity.to_string(), requests: Mutex::new(vec![]) }
}

fn schema_version(state: &Path) -> u64 {
    let text = std::fs::read_to_string(state.join("inventory.json")).unwrap();
    let value: deser_value::Value = deser_json::from_str(&text).unwrap();
    value.as_map().unwrap().get("schemaVersion").unwrap().as_u64().unwrap()
}

fn read_json(path: &Path) -> deser_value::Value {
    deser_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn metadata_only_versions_are_recorded_published_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let site = dir.path().join("site");
    let tarball = fixture_tarball();
    let stub_integrity = integrity_of(b"native-win32-tarball");
    let upstream = upstream(tarball.clone(), &stub_integrity);
    seed(&state, &tarball, &stub_integrity);
    assert_eq!(schema_version(&state), 2);

    let summary = fetch_inventory(&state, &config(), &upstream).unwrap();
    assert_eq!(summary.verified_count, 1);
    assert_eq!(summary.metadata_only_count, 2);
    assert_eq!(summary.metadata_only_recorded, 2);
    assert_eq!(schema_version(&state), 3);
    assert_eq!(upstream.requested(STUB_PACKUMENT_URL), 1);
    assert!(upstream.requests.lock().unwrap().iter().all(|u| !u.contains("native-")));

    let verified = verify_inventory(&state, &config()).unwrap();
    assert_eq!((verified.artifact_count, verified.metadata_only_count), (1, 2));

    let inventory_before = std::fs::read(state.join("inventory.json")).unwrap();
    let again = fetch_inventory(&state, &config(), &upstream).unwrap();
    assert_eq!((again.metadata_only_count, again.metadata_only_recorded), (2, 0));
    assert_eq!(upstream.requested(STUB_PACKUMENT_URL), 1);
    assert_eq!(std::fs::read(state.join("inventory.json")).unwrap(), inventory_before);

    let options = PublishOptions { state_directory: &state, site_directory: &site, config: &config(), render_base_url: None };
    let published = publish_site(&options).unwrap();
    assert!(!published.no_op);
    assert_eq!((published.package_count, published.version_count, published.metadata_only_count), (2, 1, 2));

    let current = site.join("mirror").join("current");
    let packument = read_json(&current.join("@scope").join("native"));
    let root = packument.as_map().unwrap();
    assert_eq!(root.get("dist-tags").unwrap().as_map().unwrap().len(), 0);
    let times = root.get("time").unwrap().as_map().unwrap();
    assert_eq!(times.get("1.0.0").unwrap().as_str(), Some("2024-01-02T03:04:05.678Z"));
    assert_eq!(times.get("1.1.0").unwrap().as_str(), Some("2024-02-03T04:05:06.789Z"));
    assert_eq!(times.len(), 2);
    let version = root.get("versions").unwrap().as_map().unwrap().get("1.1.0").unwrap().as_map().unwrap();
    let dist = version.get("dist").unwrap().as_map().unwrap();
    assert_eq!(dist.get("integrity").unwrap().as_str(), Some(stub_integrity.as_str()));
    assert_eq!(
        dist.get("tarball").unwrap().as_str(),
        Some("http://127.0.0.1:8080/current/@scope/native/-/native-1.1.0.tgz")
    );
    assert_eq!(dist.len(), 2);
    assert_eq!(version.get("os").unwrap().as_seq().unwrap().len(), 1);
    assert_eq!(version.get("cpu").unwrap().as_seq().unwrap().first().unwrap().as_str(), Some("x64"));
    assert!(version.get("libc").is_none());

    let hosted = read_json(&current.join("flatdemo"));
    let hosted_dist = hosted.as_map().unwrap().get("versions").unwrap().as_map().unwrap().get("1.2.3").unwrap().as_map().unwrap().get("dist").unwrap().as_map().unwrap().clone();
    assert!(hosted_dist.get("tarball").unwrap().as_str().unwrap().contains("/current/-/tarballs/"));

    let tarballs = std::fs::read_dir(site.join("mirror").join("tarballs")).unwrap().count();
    assert_eq!(tarballs, 1);
    let catalog = std::fs::read_to_string(current.join("catalog.json")).unwrap();
    assert!(catalog.contains("flatdemo") && !catalog.contains("native"));
    let llms = std::fs::read_to_string(current.join("llms.txt")).unwrap();
    assert!(llms.contains("flatdemo") && !llms.contains("native"));

    let republished = publish_site(&options).unwrap();
    assert!(republished.no_op);
    assert_eq!(republished.generation_id, published.generation_id);
}

#[test]
fn rejects_upstream_integrity_that_disagrees_with_the_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tarball = fixture_tarball();
    let upstream = upstream(tarball.clone(), &integrity_of(b"something else"));
    seed(&state, &tarball, &integrity_of(b"native-win32-tarball"));
    let before = std::fs::read(state.join("inventory.json")).unwrap();
    let err = fetch_inventory(&state, &config(), &upstream).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert_eq!(std::fs::read(state.join("inventory.json")).unwrap(), before);
}

#[test]
fn worker_errors_keep_their_conflict_category() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tarball = fixture_tarball();
    let stub_integrity = integrity_of(b"native-win32-tarball");
    let upstream = upstream(tarball, &stub_integrity);
    seed_with_flatdemo_integrity(&state, &integrity_of(b"different tarball"), &stub_integrity);
    let err = fetch_inventory(&state, &config(), &upstream).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
}

#[test]
fn worker_errors_keep_their_corrupt_category() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tarball = fixture_tarball();
    let stub_integrity = integrity_of(b"native-win32-tarball");
    let upstream = upstream(tarball.clone(), &stub_integrity);
    seed(&state, &tarball, &stub_integrity);
    let objects = state.join("objects").join("sha512");
    std::fs::create_dir_all(&objects).unwrap();
    std::fs::write(objects.join(format!("{:x}.tgz", Sha512::digest(&tarball))), b"corrupt").unwrap();
    let err = fetch_inventory(&state, &config(), &upstream).unwrap_err();
    assert!(matches!(err, Error::Corrupt(_)), "{err}");
}

#[test]
fn verify_requires_a_record_for_every_excluded_version() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let tarball = fixture_tarball();
    let stub_integrity = integrity_of(b"native-win32-tarball");
    let upstream = upstream(tarball.clone(), &stub_integrity);
    seed(&state, &tarball, &stub_integrity);
    fetch_inventory(&state, &config(), &upstream).unwrap();
    let mut inventory = load_inventory(&state).unwrap();
    inventory.metadata_only.truncate(1);
    update_inventory(&state, |_| Ok(inventory.clone())).unwrap();
    let err = verify_inventory(&state, &config()).unwrap_err();
    assert!(err.to_string().contains("no metadata-only record"), "{err}");
}
