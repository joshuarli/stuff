#![forbid(unsafe_code)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use mynpm::inventory::{
    validate_artifact_record, ArtifactRecord, Inventory, LockfileVersion, Manager, PackageFlags,
    PackageRecord, PlatformConstraints, ProjectRecord, SourceKind,
};
use mynpm::inventory_store::save_inventory;
use sha2::{Digest, Sha512};
use stuff::publish::cargo::{adopt_site, Publisher, Store};
use stuff::publish::config::Config;
use stuff::publish::npm::library_config;
use stuff::publish::pypi::project_settings;
use stuff::publish::r2::pull::{self as r2_pull, PullOptions};
use stuff::publish::r2::sync;
use stuff::publish::r2::{Credentials, R2Client, UploadOptions};
use stuff_oci::{
    parse_blobs_json, parse_repository_json, render_repository_json, Digest as OciDigest,
    RepositoryName,
};
use support::{lower_hex, seed_pypi_generation, FakeS3, ACCESS_KEY, SECRET_KEY};

const BINARY: &str = env!("CARGO_BIN_EXE_stuff");
const CARGO_BUCKET: &str = "stuff-mycrates";
const NPM_BUCKET: &str = "stuff-mynpm";
const PYPI_BUCKET: &str = "stuff-mypypi";
const OCI_BUCKET: &str = "stuff-oci";
const OCI_BLOBS: &str = "stuff-oci-blobs";

const CARGO_LIVE: &str = "https://crates.live.test";
const CARGO_FRESH: &str = "https://crates.fresh.test";
const NPM_LIVE: &str = "https://npm.live.test/";
const NPM_FRESH: &str = "https://npm.fresh.test/";
const PYPI_LIVE: &str = "https://pypi.live.test";
const PYPI_FRESH: &str = "https://pypi.fresh.test";
const OCI_PUBLIC: &str = "https://oci.live.test";

const TARBALL_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/tarballs/flatdemo-1.2.3.tgz");
const WHEEL_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mypypi/wheels/validation_fixture-1.0-py3-none-any.whl");
const OCI_LAYOUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/oci/layout");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn environment(work: &Path, fake: &FakeS3) -> Vec<(String, String)> {
    [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

fn run(work: &Path, fake: &FakeS3, args: &[&str]) -> Run {
    let output = Command::new(BINARY)
        .args(args)
        .env_clear()
        .envs(environment(work, fake))
        .output()
        .expect("run the stuff binary");
    Run {
        code: output.status.code().expect("the binary was killed"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

fn write_config(work: &Path, registry: &str, public_base_url: &str) {
    let root = work.join("config");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.json"),
        format!("{{\"{registry}\":{{\"public_base_url\":\"{public_base_url}\"}}}}\n"),
    )
    .unwrap();
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

fn client(fake: &FakeS3) -> R2Client {
    R2Client::new(credentials(fake))
}

fn remote(fake: &FakeS3, bucket: &str, key: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == bucket && object.key == key)
        .map(|object| object.body)
}

fn current(fake: &FakeS3, bucket: &str) -> String {
    String::from_utf8(remote(fake, bucket, "current").expect("current marker")).unwrap()
}

fn mutation_count(fake: &FakeS3, after: usize) -> usize {
    fake.requests()[after..]
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count()
}

fn promoted_conditionally(fake: &FakeS3, generation: &str) -> bool {
    fake.put_conditions()
        .iter()
        .any(|condition| condition.body == generation && condition.if_match.is_some())
}

/// Upload a published site tree to the fake bucket, its generation manifests, and `current`.
fn seed_site(fake: &FakeS3, bucket: &str, site: &Path, excludes: &[&str]) -> String {
    let local = sync::collect(site, &UploadOptions::default(), excludes).expect("collect the live site");
    for (key, file) in &local.files {
        let body = fs::read(site.join(&file.path)).expect("read a live site file");
        fake.seed(bucket, key, &body, "application/octet-stream");
    }
    for (key, manifest) in &local.manifests {
        fake.seed(bucket, key, &manifest.body, "application/json");
    }
    fake.seed(bucket, "current", local.generation.as_bytes(), "text/plain");
    local.generation
}

fn compare_remote_generation(
    fake: &FakeS3,
    bucket: &str,
    generation: &str,
    site: &Path,
    excludes: &[&str],
) {
    let local = sync::collect(site, &UploadOptions::default(), excludes).unwrap();
    assert_eq!(local.generation, generation);
    let prefix = format!("generations/{generation}/");
    let mut expected = BTreeMap::new();
    for (key, file) in &local.files {
        if key.starts_with(&prefix) {
            expected.insert(key.clone(), fs::read(site.join(&file.path)).unwrap());
        }
    }
    assert!(!expected.is_empty(), "the oracle generation is empty");
    for (key, bytes) in &expected {
        let remote = remote(fake, bucket, key).unwrap_or_else(|| panic!("missing {key}"));
        assert_eq!(&remote, bytes, "{key}");
    }
    for object in fake.state() {
        if object.bucket == bucket && object.key.starts_with(&prefix) {
            assert!(
                expected.contains_key(&object.key),
                "the live generation has an extra object {}",
                object.key
            );
        }
    }
}

fn package_versions(text: &str) -> Vec<(String, Vec<String>)> {
    let catalog = stuff_catalog::parse_catalog_json(text).expect("parse a catalog");
    let mut packages: Vec<(String, Vec<String>)> = catalog
        .packages
        .iter()
        .map(|package| {
            let mut versions: Vec<String> =
                package.versions.iter().map(|entry| entry.version.clone()).collect();
            versions.sort();
            (package.name.clone(), versions)
        })
        .collect();
    packages.sort();
    packages
}

fn remote_catalog(fake: &FakeS3, bucket: &str, generation: &str) -> String {
    let key = format!("generations/{generation}/catalog.json");
    String::from_utf8(remote(fake, bucket, &key).expect(&key)).unwrap()
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry.file_type().unwrap();
        if file_type.is_symlink() {
            let link = fs::read_link(&path).unwrap();
            std::os::unix::fs::symlink(link, &destination).unwrap();
        } else if file_type.is_dir() {
            copy_tree(&path, &destination);
        } else {
            fs::copy(&path, &destination).unwrap();
        }
    }
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

fn npm_artifact(name: &str, version: &str, bytes: &[u8]) -> ArtifactRecord {
    let digest = lower_hex(Sha512::digest(bytes));
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
    let value: deser_value::Value = deser_json::from_str(&text).unwrap();
    validate_artifact_record(&value).unwrap()
}

fn npm_package(record: &ArtifactRecord) -> PackageRecord {
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

fn build_npm_live(work: &Path) -> PathBuf {
    let state = work.join("npm-live-state");
    let site = work.join("npm-live-site");
    let artifacts = vec![
        npm_artifact("flatdemo", "1.2.3", &fs::read(TARBALL_FIXTURE).unwrap()),
        npm_artifact("retained-demo", "1.0.0", &tarball("retained-demo", "1.0.0")),
    ];
    let project = ProjectRecord {
        id: "live".to_string(),
        manager: Manager::Pnpm,
        package_manager_pin: None,
        lockfile_path: "pnpm-lock.yaml".to_string(),
        lockfile_version: LockfileVersion::Pnpm("9.0".to_string()),
        targets: vec!["mac-arm64".to_string()],
        importers: vec![".".to_string()],
        roots: Vec::new(),
        packages: artifacts.iter().map(npm_package).collect(),
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
    for (name, version) in [("flatdemo", "1.2.3"), ("retained-demo", "1.0.0")] {
        let bytes = if name == "flatdemo" {
            fs::read(TARBALL_FIXTURE).unwrap()
        } else {
            tarball(name, version)
        };
        fs::write(objects.join(format!("{}.tgz", lower_hex(Sha512::digest(&bytes)))), bytes).unwrap();
    }
    mynpm::publish::publish_site(&mynpm::publish::PublishOptions {
        state_directory: &state,
        site_directory: &site,
        config: &library_config(&Config::default()),
        render_base_url: Some(NPM_LIVE.to_string()),
    })
    .unwrap();
    site
}

fn materialize_objects(tarballs: &Path, state_dir: &Path) {
    let objects = state_dir.join("objects").join("sha512");
    fs::create_dir_all(&objects).unwrap();
    for entry in fs::read_dir(tarballs).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with(".tgz") {
            continue;
        }
        fs::hard_link(entry.path(), objects.join(&name)).unwrap();
    }
}

fn materialize_npm_aliases(mirror: &Path) {
    let generation = sync::active_generation(mirror).unwrap();
    let generation_dir = mirror.join("generations").join(&generation);
    let text = fs::read_to_string(generation_dir.join("manifest.json")).unwrap();
    let value: deser_value::Value = deser_json::from_str(&text).unwrap();
    let artifacts = value
        .as_map()
        .and_then(|map| map.get("artifacts"))
        .and_then(|artifacts| artifacts.as_seq())
        .unwrap();
    let digests: Vec<String> = artifacts
        .iter()
        .map(|artifact| {
            mynpm::inventory::validate_artifact_record(artifact)
                .unwrap()
                .artifact
                .sha512
        })
        .collect();
    for relative in ["tarballs", "-/tarballs"] {
        let directory = generation_dir.join(relative);
        fs::create_dir_all(&directory).unwrap();
        for digest in &digests {
            let target = directory.join(format!("{digest}.tgz"));
            if target.symlink_metadata().is_ok() {
                continue;
            }
            fs::hard_link(mirror.join("tarballs").join(format!("{digest}.tgz")), &target).unwrap();
        }
    }
}

fn build_pypi_live(work: &Path) -> PathBuf {
    let site = work.join("pypi-live-site");
    let settings = project_settings(PYPI_LIVE).unwrap();
    let (_, no_op) =
        seed_pypi_generation(Path::new(WHEEL_FIXTURE), settings.targets(), &site, PYPI_LIVE);
    assert!(!no_op);
    site
}

fn build_oci_live(work: &Path) -> PathBuf {
    let state = myoci::state::State::new(work.join("oci-live-state"));
    let repository = RepositoryName::parse("example/app").unwrap();
    myoci::import::import_layout(&state, Path::new(OCI_LAYOUT), &repository, None).unwrap();
    let site = work.join("oci-live-site");
    myoci::publish::publish(&state, &site, OCI_PUBLIC).unwrap();
    site
}

fn pull_oci_blobs(client: &R2Client, bucket: &str, site: &Path, generation: &str) {
    let blobs_path = site
        .join("generations")
        .join(generation)
        .join("blobs.json");
    let digests =
        parse_blobs_json(&fs::read_to_string(&blobs_path).unwrap()).unwrap();
    let directory = site.join("blobs/sha256");
    fs::create_dir_all(&directory).unwrap();
    for digest in digests {
        let bytes = client.get_object(bucket, &format!("sha256/{}", digest.hex())).unwrap();
        assert_eq!(OciDigest::from_bytes(&bytes), digest);
        fs::write(directory.join(digest.hex()), bytes).unwrap();
    }
}

fn cargo_oracle(work: &Path, live_site: &Path, base: &str) -> (String, PathBuf) {
    let source = work.join("cargo-oracle-input");
    copy_tree(live_site, &source);
    let store = Store::new(work.join("cargo-oracle-state"));
    adopt_site(&store, &source).unwrap();
    let site = work.join("cargo-oracle-site");
    let outcome = Publisher::new(&store, &site, base, false).publish().unwrap();
    (outcome.generation_id, site)
}

fn npm_oracle(work: &Path, fake: &FakeS3) -> (String, PathBuf) {
    let site = work.join("npm-oracle-site");
    let mirror = site.join("mirror");
    r2_pull::pull(&client(fake), NPM_BUCKET, &mirror, PullOptions::default()).unwrap();
    let state = work.join("npm-oracle-state");
    materialize_objects(&mirror.join("tarballs"), &state);
    materialize_npm_aliases(&mirror);
    let mut config = Config::default();
    config.npm.public_base_url = NPM_FRESH.to_string();
    let summary = mynpm::publish::publish_site(&mynpm::publish::PublishOptions {
        state_directory: &state,
        site_directory: &site,
        config: &library_config(&config),
        render_base_url: None,
    })
    .unwrap();
    (summary.generation_id, mirror)
}

fn pypi_oracle(work: &Path, fake: &FakeS3) -> (String, PathBuf) {
    let site = work.join("pypi-oracle-site");
    r2_pull::pull(&client(fake), PYPI_BUCKET, &site, PullOptions::default()).unwrap();
    let summary = mypypi::publish::republish(&mypypi::publish::RepublishOptions {
        output: &site,
        public_url: PYPI_FRESH,
    })
    .unwrap();
    (summary.generation_id, site)
}

fn oci_oracle(work: &Path, fake: &FakeS3) -> (String, PathBuf) {
    let site = work.join("oci-oracle-site");
    let report = r2_pull::pull(&client(fake), OCI_BUCKET, &site, PullOptions::default()).unwrap();
    pull_oci_blobs(&client(fake), OCI_BLOBS, &site, &report.generation);
    let state = myoci::state::State::new(work.join("oci-oracle-state"));
    myoci::publish::adopt(&state, &site).unwrap();
    let output = work.join("oci-oracle-output");
    let summary = myoci::publish::publish(&state, &output, OCI_PUBLIC).unwrap();
    (summary.generation, output)
}

#[test]
fn republish_cargo_rerenders_the_live_publication_and_promotes_conditionally() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();

    let source = work.path().join("golden-input");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/mycrates/tests/golden/adopt-publish/input-site"),
        &source,
    );
    let store = Store::new(work.path().join("cargo-live-state"));
    adopt_site(&store, &source).unwrap();
    let live_site = work.path().join("cargo-live-site");
    let live = Publisher::new(&store, &live_site, CARGO_LIVE, false).publish().unwrap();
    assert!(!live.generation_id.is_empty());
    let seeded = seed_site(&fake, CARGO_BUCKET, &live_site, &[]);
    assert_eq!(seeded, live.generation_id);

    write_config(work.path(), "cargo", CARGO_FRESH);
    let result = run(work.path(), &fake, &["republish", "cargo"]);
    assert_eq!(result.code, 0, "stdout={}\nstderr={}", result.stdout, result.stderr);

    let (oracle_id, oracle_site) = cargo_oracle(work.path(), &live_site, CARGO_FRESH);
    assert_ne!(oracle_id, live.generation_id);
    assert_eq!(current(&fake, CARGO_BUCKET), oracle_id);
    compare_remote_generation(&fake, CARGO_BUCKET, &oracle_id, &oracle_site, &[]);
    assert!(
        promoted_conditionally(&fake, &oracle_id),
        "the promotion was not conditional: {:?}",
        fake.put_conditions()
    );
    assert_eq!(
        package_versions(&remote_catalog(&fake, CARGO_BUCKET, &oracle_id)),
        package_versions(&remote_catalog(&fake, CARGO_BUCKET, &live.generation_id)),
        "republish changed the package set"
    );

    let requests = fake.requests().len();
    let conditions = fake.put_conditions().len();
    let rerun = run(work.path(), &fake, &["republish", "cargo", "--allow-drops"]);
    assert_eq!(rerun.code, 0, "stdout={}\nstderr={}", rerun.stdout, rerun.stderr);
    assert_eq!(current(&fake, CARGO_BUCKET), oracle_id);
    assert_eq!(mutation_count(&fake, requests), 0, "the rerun mutated the bucket");
    assert_eq!(fake.put_conditions().len(), conditions, "the rerun promoted");
}

#[test]
fn republish_npm_rerenders_the_live_publication_and_promotes_conditionally() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();

    let live_site = build_npm_live(work.path());
    let live_id = seed_site(&fake, NPM_BUCKET, &live_site.join("mirror"), &[]);

    write_config(work.path(), "npm", NPM_FRESH);
    let result = run(work.path(), &fake, &["republish", "npm"]);
    assert_eq!(result.code, 0, "stdout={}\nstderr={}", result.stdout, result.stderr);

    let (oracle_id, oracle_mirror) = npm_oracle(work.path(), &fake);
    assert_ne!(oracle_id, live_id);
    assert_eq!(current(&fake, NPM_BUCKET), oracle_id);
    compare_remote_generation(&fake, NPM_BUCKET, &oracle_id, &oracle_mirror, &[]);
    assert!(
        promoted_conditionally(&fake, &oracle_id),
        "the promotion was not conditional: {:?}",
        fake.put_conditions()
    );
    assert_eq!(
        package_versions(&remote_catalog(&fake, NPM_BUCKET, &oracle_id)),
        package_versions(&remote_catalog(&fake, NPM_BUCKET, &live_id)),
        "republish changed the package set"
    );

    let requests = fake.requests().len();
    let conditions = fake.put_conditions().len();
    let rerun = run(work.path(), &fake, &["republish", "npm"]);
    assert_eq!(rerun.code, 0, "stdout={}\nstderr={}", rerun.stdout, rerun.stderr);
    assert_eq!(current(&fake, NPM_BUCKET), oracle_id);
    assert_eq!(mutation_count(&fake, requests), 0, "the rerun mutated the bucket");
    assert_eq!(fake.put_conditions().len(), conditions, "the rerun promoted");
}

#[test]
fn republish_pypi_rerenders_the_live_publication_and_promotes_conditionally() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();

    let live_site = build_pypi_live(work.path());
    let live_id = seed_site(&fake, PYPI_BUCKET, &live_site, &[]);

    write_config(work.path(), "pypi", PYPI_FRESH);
    let result = run(work.path(), &fake, &["republish", "pypi"]);
    assert_eq!(result.code, 0, "stdout={}\nstderr={}", result.stdout, result.stderr);

    let (oracle_id, oracle_site) = pypi_oracle(work.path(), &fake);
    assert_ne!(oracle_id, live_id);
    assert_eq!(current(&fake, PYPI_BUCKET), oracle_id);
    compare_remote_generation(&fake, PYPI_BUCKET, &oracle_id, &oracle_site, &[]);
    assert!(
        promoted_conditionally(&fake, &oracle_id),
        "the promotion was not conditional: {:?}",
        fake.put_conditions()
    );
    assert_eq!(
        package_versions(&remote_catalog(&fake, PYPI_BUCKET, &oracle_id)),
        package_versions(&remote_catalog(&fake, PYPI_BUCKET, &live_id)),
        "republish changed the package set"
    );

    let requests = fake.requests().len();
    let conditions = fake.put_conditions().len();
    let rerun = run(work.path(), &fake, &["republish", "pypi"]);
    assert_eq!(rerun.code, 0, "stdout={}\nstderr={}", rerun.stdout, rerun.stderr);
    assert_eq!(current(&fake, PYPI_BUCKET), oracle_id);
    assert_eq!(mutation_count(&fake, requests), 0, "the rerun mutated the bucket");
    assert_eq!(fake.put_conditions().len(), conditions, "the rerun promoted");
}

#[test]
fn republish_oci_rerenders_the_live_publication_and_promotes_conditionally() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();

    let live_site = build_oci_live(work.path());
    let live_id = {
        let target = fs::read_link(live_site.join("current")).unwrap();
        target.to_string_lossy().trim_start_matches("generations/").to_string()
    };
    let generation_dir = live_site.join("generations").join(&live_id);
    let repository = RepositoryName::parse("example/app").unwrap();
    let repository_path = generation_dir.join("repositories/example/app/repository.json");
    let mut view =
        parse_repository_json(&repository, &fs::read_to_string(&repository_path).unwrap()).unwrap();
    let (digest, descriptor) = view
        .manifests
        .iter()
        .next()
        .map(|(digest, descriptor)| (digest.clone(), descriptor.clone()))
        .expect("the live repository view lists a manifest");
    let mut extra = fs::read(live_site.join("oci/manifests/sha256").join(digest.hex())).unwrap();
    extra.push(b'\n');
    let extra_digest = OciDigest::from_bytes(&extra);
    let mut extra_descriptor = descriptor;
    extra_descriptor.digest = extra_digest.clone();
    extra_descriptor.size = extra.len() as u64;
    view.insert_manifest(extra_descriptor);
    fs::write(&repository_path, render_repository_json(&view)).unwrap();
    fs::write(
        live_site.join("oci/manifests/sha256").join(extra_digest.hex()),
        &extra,
    )
    .unwrap();

    let seeded = seed_site(&fake, OCI_BUCKET, &live_site, &["blobs/"]);
    assert_eq!(seeded, live_id);
    for entry in fs::read_dir(live_site.join("blobs/sha256")).unwrap() {
        let entry = entry.unwrap();
        fake.seed(
            OCI_BLOBS,
            &format!("sha256/{}", entry.file_name().to_string_lossy()),
            &fs::read(entry.path()).unwrap(),
            "application/octet-stream",
        );
    }

    write_config(work.path(), "oci", OCI_PUBLIC);
    let result = run(work.path(), &fake, &["republish", "oci"]);
    assert_eq!(result.code, 0, "stdout={}\nstderr={}", result.stdout, result.stderr);

    let (oracle_id, oracle_site) = oci_oracle(work.path(), &fake);
    assert_ne!(oracle_id, live_id);
    assert_eq!(current(&fake, OCI_BUCKET), oracle_id);
    compare_remote_generation(&fake, OCI_BUCKET, &oracle_id, &oracle_site, &["blobs/"]);
    assert!(
        promoted_conditionally(&fake, &oracle_id),
        "the promotion was not conditional: {:?}",
        fake.put_conditions()
    );
    assert_eq!(
        package_versions(&remote_catalog(&fake, OCI_BUCKET, &oracle_id)),
        package_versions(&remote_catalog(&fake, OCI_BUCKET, &live_id)),
        "republish changed the package set"
    );

    let requests = fake.requests().len();
    let conditions = fake.put_conditions().len();
    let rerun = run(work.path(), &fake, &["republish", "oci"]);
    assert_eq!(rerun.code, 0, "stdout={}\nstderr={}", rerun.stdout, rerun.stderr);
    assert_eq!(current(&fake, OCI_BUCKET), oracle_id);
    assert_eq!(mutation_count(&fake, requests), 0, "the rerun mutated the bucket");
    assert_eq!(fake.put_conditions().len(), conditions, "the rerun promoted");
}

#[test]
fn allow_drops_is_only_accepted_by_republish() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let rejected = run(
        work.path(),
        &fake,
        &["publish", "cargo", "-p", "anything", "--allow-drops"],
    );
    assert_eq!(rejected.code, 2, "stdout={}\nstderr={}", rejected.stdout, rejected.stderr);
    assert!(
        rejected.stderr.contains("--allow-drops"),
        "the parser accepted --allow-drops on publish: {}",
        rejected.stderr
    );
}

#[test]
fn make_dry_runs_resolve_stuff_republish() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let registries = [
        ("republish-mycrates", "cargo"),
        ("republish-mynpm", "npm"),
        ("republish-mypypi", "pypi"),
        ("republish-oci", "oci"),
    ];
    for (target, registry) in registries {
        let output = Command::new("make")
            .arg("-n")
            .arg(target)
            .current_dir(&root)
            .output()
            .expect("run make -n");
        assert!(output.status.success(), "make -n {target} failed");
        let text = String::from_utf8_lossy(&output.stdout);
        let needle = format!("stuff republish {registry}");
        assert!(
            text.lines().any(|line| line.contains(&needle)),
            "make -n {target} did not resolve {needle}: {text}"
        );
    }
    let output = Command::new("make")
        .arg("-n")
        .arg("deploy")
        .current_dir(&root)
        .output()
        .expect("run make -n deploy");
    assert!(output.status.success(), "make -n deploy failed");
    let text = String::from_utf8_lossy(&output.stdout);
    for registry in ["cargo", "npm", "pypi"] {
        let needle = format!("stuff republish {registry}");
        assert!(
            text.lines().any(|line| line.contains(&needle)),
            "make -n deploy did not resolve {needle}: {text}"
        );
    }
}
