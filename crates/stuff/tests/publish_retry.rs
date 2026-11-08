#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use mynpm::inventory::{
    validate_artifact_record, ArtifactRecord, Inventory, LockfileVersion, Manager, PackageFlags,
    PackageRecord, PlatformConstraints, ProjectRecord, SourceKind,
};
use mynpm::inventory_store::save_inventory;
use mynpm::publish::{publish_site, PublishOptions};
use sha2::{Digest, Sha512};
use stuff::publish::cargo::names::{lower_name, shard_for_name};
use stuff::publish::config::Config;
use stuff::publish::npm;
use stuff::publish::r2::pull::md5_hex;
use stuff::publish::{
    CargoPublish, Environment, NpmPublish, Options, PublishRequest, Request, Status,
};
use support::{lower_hex, ACCESS_KEY, FakeS3, SECRET_KEY};

const CARGO_BUCKET: &str = "stuff-mycrates";
const NPM_BUCKET: &str = "stuff-mynpm";
const FIXTURE: &str = "stuff-cargo-publish-fixture";
const H1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const H3: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const NPM_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/tarballs/flatdemo-1.2.3.tgz");
const LOCK_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/locks/package-lock-v2.json");

fn base_pairs() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(name, _)| !name.starts_with("STUFF_") && !name.starts_with("R2_"))
        .collect()
}

fn cargo_environment(work: &Path, fake: &FakeS3) -> Environment {
    let mut pairs = base_pairs();
    for (name, value) in [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
        ("CARGO_HTTP_PROXY", "http://127.0.0.1:1".to_string()),
        ("NO_PROXY", "127.0.0.1,localhost".to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
}

fn npm_environment(work: &Path, fake: &FakeS3) -> Environment {
    let mut pairs = base_pairs();
    for (name, value) in [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
}

fn copy_fixture_crate(work: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cargo-publish");
    let target = work.join("crate");
    copy_tree(&source, &target);
    target.join("Cargo.toml")
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create copy target");
    for entry in fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("source entry");
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("source type").is_dir() {
            copy_tree(&path, &destination);
        } else {
            fs::copy(&path, &destination).expect("copy fixture file");
        }
    }
}

fn cargo_request(manifest: &Path) -> Request {
    Request::Publish {
        request: PublishRequest::Cargo(CargoPublish {
            packages: vec![FIXTURE.to_string()],
            manifest_path: Some(manifest.display().to_string()),
            registry: Some("stuff-e2e".to_string()),
        }),
        options: Options::default(),
    }
}

fn index_line(name: &str, vers: &str, checksum: &str) -> String {
    format!(
        "{{\"name\":\"{name}\",\"vers\":\"{vers}\",\"deps\":[],\"cksum\":\"{checksum}\",\"features\":{{}},\"yanked\":false}}\n"
    )
}

fn seed_index(
    fake: &FakeS3,
    generation: &str,
    view: &str,
    name: &str,
    vers: &str,
    checksum: &str,
) {
    let shard = shard_for_name(&lower_name(name)).expect("fixture crate name shards");
    let key = format!("generations/{generation}/{view}/{shard}");
    fake.seed(
        CARGO_BUCKET,
        &key,
        index_line(name, vers, checksum).as_bytes(),
        "text/plain",
    );
}

fn seed_catalog(fake: &FakeS3, generation: &str, entries: &[(&str, &str, &str)]) {
    let packages: Vec<String> = entries
        .iter()
        .map(|(name, source, version)| {
            format!(
                "{{\"name\":\"{name}\",\"source\":\"{source}\",\"versions\":[{{\"version\":\"{version}\"}}]}}"
            )
        })
        .collect();
    fake.seed(
        CARGO_BUCKET,
        &format!("generations/{generation}/catalog.json"),
        format!("{{\"packages\":[{}]}}\n", packages.join(",")).as_bytes(),
        "application/json",
    );
}

fn seed_blob(fake: &FakeS3, checksum: &str, body: &[u8]) {
    fake.seed(
        CARGO_BUCKET,
        &format!("blobs/{checksum}.crate"),
        body,
        "application/octet-stream",
    );
}

fn seed_cargo_live(fake: &FakeS3) {
    seed_blob(fake, H1, b"upstream one");
    seed_index(fake, "g1", "crates-io", "upstream-demo", "1.0.0", H1);
    seed_catalog(fake, "g1", &[("upstream-demo", "upstream", "1.0.0")]);
    fake.seed(CARGO_BUCKET, "current", b"g1", "text/plain");
}

fn seed_cargo_competing(fake: &FakeS3, extra: &str, extra_checksum: &str, view: &str) {
    seed_blob(fake, H1, b"upstream one");
    seed_blob(fake, extra_checksum, b"competing object");
    seed_index(fake, "g2", "crates-io", "upstream-demo", "1.0.0", H1);
    seed_index(fake, "g2", view, extra, "0.1.0", extra_checksum);
    seed_catalog(
        fake,
        "g2",
        &[
            ("upstream-demo", "upstream", "1.0.0"),
            (extra, if view == "registry" { "native" } else { "upstream" }, "0.1.0"),
        ],
    );
}

fn remote_body(fake: &FakeS3, bucket: &str, key: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == bucket && object.key == key)
        .map(|object| object.body)
}

fn blob_gets(fake: &FakeS3, after: usize) -> Vec<String> {
    fake.requests()
        .into_iter()
        .skip(after)
        .filter(|request| {
            request.method == "GET"
                && request.path.starts_with(&format!("/{CARGO_BUCKET}/blobs/"))
        })
        .map(|request| request.path)
        .collect()
}

#[test]
fn a_competing_additive_generation_is_merged_and_published() {
    let fake = FakeS3::start_retry();
    let work = tempfile::tempdir().expect("work directory");
    seed_cargo_live(&fake);
    seed_cargo_competing(&fake, "retained-demo", H2, "crates-io");
    fake.reject_current(1, Some("g2"));
    let manifest = copy_fixture_crate(work.path());
    let environment = cargo_environment(work.path(), &fake);
    let requests = fake.requests().len();

    let outcome = stuff::publish::run(cargo_request(&manifest), &environment)
        .expect("cargo publish after the competing generation");

    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert_eq!(outcome.packages, vec![format!("{FIXTURE}@0.1.0")]);
    let current = remote_body(&fake, CARGO_BUCKET, "current").expect("current marker");
    let generation = String::from_utf8(current).unwrap().trim().to_string();
    assert_ne!(generation, "g1");
    assert_ne!(generation, "g2");

    let catalog =
        remote_body(&fake, CARGO_BUCKET, &format!("generations/{generation}/catalog.json"))
            .expect("published catalog");
    let catalog = String::from_utf8(catalog).unwrap();
    assert!(catalog.contains("retained-demo"), "{catalog}");
    assert!(catalog.contains("upstream-demo"), "{catalog}");
    assert!(catalog.contains(FIXTURE), "{catalog}");

    let retained_shard = shard_for_name(&lower_name("retained-demo")).unwrap();
    assert!(
        remote_body(
            &fake,
            CARGO_BUCKET,
            &format!("generations/{generation}/crates-io/{retained_shard}")
        )
        .is_some(),
        "the retried generation did not adopt the competing index"
    );

    let conditions = fake.put_conditions();
    assert_eq!(conditions.len(), 2, "{conditions:?}");
    assert_eq!(
        conditions[0].if_match,
        Some(format!("\"{}\"", md5_hex(b"g1")))
    );
    assert_eq!(
        conditions[1].if_match,
        Some(format!("\"{}\"", md5_hex(b"g2")))
    );
    assert_eq!(conditions[1].body, generation);
    assert_eq!(
        blob_gets(&fake, requests),
        Vec::<String>::new(),
        "the retried publish downloaded shared blobs in the live listing"
    );
}

#[test]
fn a_byte_conflicting_competing_generation_exits_three() {
    let fake = FakeS3::start_retry();
    let work = tempfile::tempdir().expect("work directory");
    seed_cargo_live(&fake);
    seed_cargo_competing(&fake, FIXTURE, H3, "registry");
    fake.reject_current(1, Some("g2"));
    let manifest = copy_fixture_crate(work.path());
    let environment = cargo_environment(work.path(), &fake);

    let error = stuff::publish::run(cargo_request(&manifest), &environment)
        .expect_err("a byte conflict is refused");

    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(error.to_string().contains(FIXTURE), "{error}");
    assert_eq!(
        remote_body(&fake, CARGO_BUCKET, "current").unwrap(),
        b"g2"
    );
    assert_eq!(fake.put_conditions().len(), 1);
}

#[test]
fn three_conflict_retries_are_exhausted_with_exit_three() {
    let fake = FakeS3::start_retry();
    let work = tempfile::tempdir().expect("work directory");
    seed_cargo_live(&fake);
    fake.reject_current(4, None);
    let manifest = copy_fixture_crate(work.path());
    let environment = cargo_environment(work.path(), &fake);

    let error = stuff::publish::run(cargo_request(&manifest), &environment)
        .expect_err("every promotion attempt is stale");

    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(
        error.to_string().contains("current changed during promotion"),
        "{error}"
    );
    assert_eq!(fake.put_conditions().len(), 4);
    assert_eq!(
        remote_body(&fake, CARGO_BUCKET, "current").unwrap(),
        b"g1"
    );
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
    let value: deser_value::Value = deser_json::from_str(&text).unwrap();
    validate_artifact_record(&value).unwrap()
}

fn tarball(name: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
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

fn build_live(work: &Path, packages: &[(&str, &str, Vec<u8>)]) -> PathBuf {
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
    publish_site(&PublishOptions {
        state_directory: &state,
        site_directory: &site,
        config: &npm::library_config(&Config::default()),
        render_base_url: Some("http://127.0.0.1:9/mirror/".to_string()),
    })
    .unwrap();
    site
}

fn is_alias(relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    parts.len() >= 3
        && parts[0] == "generations"
        && (parts[2] == "tarballs" || (parts[2] == "-" && parts.get(3) == Some(&"tarballs")))
}

fn seed_mirror(fake: &FakeS3, site: &Path, source_generation: &str, target: &str) -> usize {
    let mirror = site.join("mirror");
    let source_prefix = format!("generations/{source_generation}/");
    let target_prefix = format!("generations/{target}/");
    let mut seeded = 0;
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
            let key = match relative.strip_prefix(&source_prefix) {
                Some(rest) => format!("{target_prefix}{rest}"),
                None => relative.clone(),
            };
            let body = fs::read(&path).unwrap();
            fake.seed(NPM_BUCKET, &key, &body, "application/octet-stream");
            seeded += 1;
        }
    }
    seeded
}

fn npm_lockfile(work: &Path, tarball: &[u8]) -> PathBuf {
    let digest = lower_hex(Sha512::digest(tarball));
    let raw: Vec<u8> = (0..digest.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&digest[index..index + 2], 16).unwrap())
        .collect();
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(&raw)
    );
    let text = fs::read_to_string(LOCK_FIXTURE).unwrap();
    let placeholder = "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
    assert!(text.contains(placeholder), "the lock fixture changed shape");
    let text = text.replace(placeholder, &integrity);
    let path = work.join("package-lock.json");
    fs::write(&path, text).unwrap();
    path
}

fn npm_request(lockfile: &Path) -> Request {
    Request::Publish {
        request: PublishRequest::Npm(NpmPublish {
            lockfile: lockfile.to_string_lossy().into_owned(),
            project: "demo".to_string(),
        }),
        options: Options::default(),
    }
}

#[test]
fn a_competing_additive_npm_generation_is_merged_and_published() {
    let fake = FakeS3::start_retry();
    let work = tempfile::tempdir().expect("work directory");
    let a = fs::read(NPM_FIXTURE).unwrap();
    let b = tarball("retained-demo", "1.0.0");
    let g1_root = work.path().join("g1");
    let g2_root = work.path().join("g2");
    fs::create_dir_all(&g1_root).unwrap();
    fs::create_dir_all(&g2_root).unwrap();
    let g1_site = build_live(&g1_root, &[("flatdemo", "1.2.3", a.clone())]);
    let g2_site = build_live(
        &g2_root,
        &[("flatdemo", "1.2.3", a.clone()), ("retained-demo", "1.0.0", b)],
    );
    let g1_generation = {
        let target = fs::read_link(g1_site.join("mirror/current")).unwrap();
        target.to_string_lossy().trim_start_matches("generations/").to_string()
    };
    let g2_generation = {
        let target = fs::read_link(g2_site.join("mirror/current")).unwrap();
        target.to_string_lossy().trim_start_matches("generations/").to_string()
    };
    let seeded = seed_mirror(&fake, &g1_site, &g1_generation, &g1_generation);
    assert!(seeded > 0, "the live generation seeded no objects");
    seed_mirror(&fake, &g2_site, &g2_generation, &g2_generation);
    fake.seed(
        NPM_BUCKET,
        "current",
        g1_generation.as_bytes(),
        "text/plain",
    );
    fake.reject_current(1, Some(&g2_generation));

    let lockfile = npm_lockfile(work.path(), &a);
    let environment = npm_environment(work.path(), &fake);
    let outcome = stuff::publish::run(npm_request(&lockfile), &environment)
        .expect("npm publish after the competing generation");

    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert_eq!(outcome.packages, vec!["flatdemo@1.2.3".to_string()]);
    let current = remote_body(&fake, NPM_BUCKET, "current").expect("current marker");
    let generation = String::from_utf8(current).unwrap().trim().to_string();
    assert_ne!(generation, g1_generation);
    assert_ne!(generation, g2_generation);
    let manifest =
        remote_body(&fake, NPM_BUCKET, &format!("generations/{generation}/manifest.json"))
            .expect("published manifest");
    let manifest = String::from_utf8(manifest).unwrap();
    assert!(manifest.contains("retained-demo"), "{manifest}");
    assert!(manifest.contains("flatdemo"), "{manifest}");
    let conditions = fake.put_conditions();
    assert_eq!(conditions.len(), 2, "{conditions:?}");
    assert_eq!(
        conditions[0].if_match,
        Some(format!("\"{}\"", md5_hex(g1_generation.as_bytes())))
    );
    assert_eq!(
        conditions[1].if_match,
        Some(format!("\"{}\"", md5_hex(g2_generation.as_bytes())))
    );
    assert_eq!(conditions[1].body, generation);
}
