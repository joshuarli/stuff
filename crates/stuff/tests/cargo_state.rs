#![forbid(unsafe_code)]

mod support;

use rustc_hash::FxHashMap;
use std::fs;
use std::path::{Path, PathBuf};

use deser_value::Value;
use stuff::publish::cargo::wirejson::json_string;
use stuff::publish::cargo::Store;
use stuff::publish::state::{BACKUP_DIRECTORY, CargoLayout};
use stuff::publish::{CargoPublish, Environment, Options, PublishRequest, Status};
use support::{ACCESS_KEY, FakeS3, SECRET_KEY};

const PUBLIC_BASE_URL: &str = "https://crates.example.test";
const REGISTRY: &str = "stuff-e2e";
const PACKAGE: &str = "stuff-cargo-publish-fixture";
const SECOND_PACKAGE: &str = "stuff-cargo-publish-fixture-two";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    manifest_dir().join("../..")
}

fn fixture_crate() -> PathBuf {
    manifest_dir().join("tests/fixtures/cargo-publish")
}

fn scratch_root() -> tempfile::TempDir {
    let base = repo_root().join(".work/tmp");
    fs::create_dir_all(&base).expect("create .work/tmp");
    tempfile::Builder::new()
        .prefix("stuff-cargo-state.")
        .tempdir_in(&base)
        .expect("scratch directory under .work/tmp")
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

fn write_config(work: &Path) {
    let config_root = work.join("config");
    fs::create_dir_all(&config_root).expect("create config root");
    fs::write(
        config_root.join("config.json"),
        format!(
            "{{\"cargo\":{{\"bucket\":\"stuff-mycrates\",\"public_base_url\":{}}}}}\n",
            json_string(PUBLIC_BASE_URL)
        ),
    )
    .expect("write stuff config");
}

fn base_environment(work: &Path, legacy: &Path) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| !name.starts_with("STUFF_") && !name.starts_with("R2_"))
        .filter(|(name, _)| name != "PWD")
        .collect();
    for (name, value) in [
        ("PWD", work.display().to_string()),
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
        ("STUFF_MYCRATES_STATE", legacy.display().to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    pairs
}

fn environment(work: &Path, legacy: &Path, fake: &FakeS3) -> Environment {
    let mut pairs = base_environment(work, legacy);
    for (name, value) in [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("R2_S3_REGION", "auto".to_string()),
        ("CARGO_HTTP_PROXY", "http://127.0.0.1:1".to_string()),
        ("NO_PROXY", "127.0.0.1,localhost".to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
}

fn publish(name: &str, manifest: &Path) -> stuff::publish::Request {
    stuff::publish::Request::Publish {
        request: PublishRequest::Cargo(CargoPublish {
            packages: vec![name.to_string()],
            manifest_path: Some(manifest.display().to_string()),
            registry: Some(REGISTRY.to_string()),
        }),
        options: Options::default(),
    }
}

fn dry_run(name: &str, manifest: &Path) -> stuff::publish::Request {
    match publish(name, manifest) {
        stuff::publish::Request::Publish { request, mut options } => {
            options.dry_run = true;
            stuff::publish::Request::Publish { request, options }
        }
        other => other,
    }
}

fn fixture_manifest(work: &Path) -> PathBuf {
    let crate_dir = work.join("crate");
    copy_tree(&fixture_crate(), &crate_dir);
    crate_dir.join("Cargo.toml")
}

fn second_crate(work: &Path) -> PathBuf {
    let crate_dir = work.join("crate-two");
    copy_tree(&fixture_crate(), &crate_dir);
    fs::write(
        crate_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{SECOND_PACKAGE}\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"Second fixture crate for the stuff cargo state test\"\nlicense = \"Apache-2.0\"\n\n[workspace]\n"
        ),
    )
    .expect("write the second fixture manifest");
    crate_dir.join("Cargo.toml")
}

fn generation_id(site: &Path) -> String {
    let target = fs::read_link(site.join("current")).expect("current symlink");
    target
        .to_str()
        .and_then(|text| text.strip_prefix("generations/"))
        .expect("current points into generations")
        .to_string()
}

fn generation_files(site: &Path, generation: &str) -> FxHashMap<PathBuf, Vec<u8>> {
    let mut files = FxHashMap::default();
    let root = site.join("generations").join(generation);
    collect_files(&root, &root, &mut files);
    files
}

fn collect_files(root: &Path, directory: &Path, files: &mut FxHashMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(directory).expect("read generation directory") {
        let entry = entry.expect("generation entry");
        let path = entry.path();
        if entry.file_type().expect("generation type").is_dir() {
            collect_files(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root).expect("relative path").to_path_buf(),
                fs::read(&path).expect("read generation file"),
            );
        }
    }
}

fn tree(root: &Path) -> FxHashMap<PathBuf, Vec<u8>> {
    let mut files = FxHashMap::default();
    if root.exists() {
        collect_tree(root, root, &mut files);
    }
    files
}

fn collect_tree(root: &Path, directory: &Path, files: &mut FxHashMap<PathBuf, Vec<u8>>) {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .expect("read tree directory")
        .map(|entry| entry.expect("tree entry"))
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().expect("tree file type");
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_tree(root, &path, files);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        files.insert(
            path.strip_prefix(root).expect("relative tree path").to_path_buf(),
            fs::read(&path).expect("read tree file"),
        );
    }
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

fn put_paths(fake: &FakeS3, after: usize) -> Vec<String> {
    fake.requests()
        .into_iter()
        .skip(after)
        .filter(|request| request.method == "PUT")
        .map(|request| request.path)
        .collect()
}

fn blob_downloads(fake: &FakeS3, after: usize) -> Vec<String> {
    fake.requests()
        .into_iter()
        .skip(after)
        .filter(|request| {
            request.method == "GET" && request.path.starts_with("/stuff-mycrates/blobs/")
        })
        .map(|request| request.path)
        .collect()
}

fn legacy_state(root: &Path) {
    let native = format!(
        "{{\"name\":\"alpha\",\"vers\":\"0.2.0\",\"checksum\":\"{}\",\"yanked\":false,\"entry\":{{\"name\":\"alpha\",\"vers\":\"0.2.0\",\"deps\":[],\"cksum\":\"{}\",\"features\":{{}},\"yanked\":false}}}}",
        "a".repeat(64),
        "a".repeat(64)
    );
    let upstream = format!(
        "{{\"name\":\"serde\",\"vers\":\"1.0.0\",\"checksum\":\"{}\",\"raw_index_line\":\"{{\\\"name\\\":\\\"serde\\\",\\\"vers\\\":\\\"1.0.0\\\"}}\",\"acquired_at\":123}}",
        "b".repeat(64)
    );
    let pins = format!(
        "{{\"project\":\"demo\",\"lock_path\":\"Cargo.lock\",\"lock_content\":\"[package]\",\"registry\":[{{\"name\":\"alpha\",\"version\":\"0.2.0\",\"checksum\":\"{}\",\"source\":\"registry+crates\"}}],\"unsupported\":[]}}",
        "a".repeat(64)
    );
    for (relative, body) in [
        ("native/alpha/0.2.0.json", native),
        ("upstream/serde/1.0.0.json", upstream),
        ("pins/demo.json", pins),
    ] {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).expect("create legacy record directory");
        fs::write(path, body).expect("write legacy record");
    }
    fs::create_dir_all(root.join("blobs")).expect("create legacy blob directory");
    fs::write(root.join(format!("blobs/{}.crate", "a".repeat(64))), b"archive")
        .expect("write legacy blob");
}

#[test]
fn a_fresh_machine_rebuilds_the_live_generation_and_fetches_only_referenced_blobs() {
    let fake = FakeS3::start_verified();

    let first = scratch_root();
    write_config(first.path());
    let legacy_first = first.path().join("absent-legacy");
    let first_environment = environment(first.path(), &legacy_first, &fake);
    let service_first = stuff::publish::run(
        publish(PACKAGE, &fixture_manifest(first.path())),
        &first_environment,
    )
    .expect("first cargo publish");
    assert_eq!(service_first.status, Status::Published);
    let first_site = first.path().join("cache/cargo/site");
    let first_generation = generation_id(&first_site);
    let first_files = generation_files(&first_site, &first_generation);
    let referenced: Vec<String> = fs::read_dir(first_site.join("blobs"))
        .expect("read the first publish blobs")
        .map(|entry| entry.expect("blob entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(referenced.len(), 1, "{referenced:?}");
    let orphan = "f".repeat(64);
    fake.seed(
        "stuff-mycrates",
        &format!("blobs/{orphan}.crate"),
        b"unreferenced shared object",
        "application/octet-stream",
    );
    let remote_before = remote_snapshot(&fake);

    let second = scratch_root();
    write_config(second.path());
    let legacy_second = second.path().join("absent-legacy");
    let second_environment = environment(second.path(), &legacy_second, &fake);
    let manifest = second.path().join("crate/Cargo.toml");
    copy_tree(&fixture_crate(), &second.path().join("crate"));
    let requests_before = fake.requests().len();
    let outcome = stuff::publish::run(publish(PACKAGE, &manifest), &second_environment)
        .expect("fresh-machine cargo publish");
    assert_eq!(outcome.status, Status::Unchanged);

    let second_site = second.path().join("cache/cargo/site");
    let second_generation = generation_id(&second_site);
    assert_eq!(second_generation, first_generation);
    assert_eq!(
        generation_files(&second_site, &second_generation),
        first_files,
        "the fresh machine rebuilt different generation bytes"
    );
    assert_eq!(
        remote_snapshot(&fake),
        remote_before,
        "the fresh-machine no-op changed the remote"
    );
    let downloads = blob_downloads(&fake, requests_before);
    assert_eq!(
        downloads,
        Vec::<String>::new(),
        "the fresh machine fetched shared blobs already present in the live listing"
    );
    assert!(
        !second_site
            .join("blobs")
            .join(format!("{orphan}.crate"))
            .exists(),
        "the fresh machine materialized an unreferenced shared object"
    );
}

#[test]
fn an_existing_mycrates_state_is_backed_up_and_preserved() {
    let work = scratch_root();
    let legacy = work.path().join("legacy-state");
    legacy_state(&legacy);
    let environment = Environment::from_pairs(base_environment(work.path(), &legacy));
    let layout = CargoLayout::from_environment(&environment);

    let report = layout.prepare(&environment).expect("migrate the legacy state");
    assert_eq!(report.blobs, 1);
    assert_eq!(report.native, 1);
    assert_eq!(report.upstream, 1);
    assert_eq!(report.pins, 1);
    assert_eq!(report.converted, 0);
    assert_eq!(report.source.as_deref(), Some(legacy.as_path()));

    let target = work.path().join("state/cargo");
    assert_eq!(
        report.backup.as_deref(),
        Some(target.join(BACKUP_DIRECTORY).as_path())
    );
    for relative in [
        "pins/demo.json",
        "native/alpha/0.2.0.json",
        "upstream/serde/1.0.0.json",
        "blobs/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.crate",
    ] {
        let original = legacy.join(relative);
        assert!(original.is_file(), "the legacy {relative} was removed");
        assert_eq!(
            fs::read(&original).unwrap(),
            fs::read(target.join(relative)).unwrap(),
            "{relative}"
        );
        assert_eq!(
            fs::read(&original).unwrap(),
            fs::read(target.join(BACKUP_DIRECTORY).join(relative)).unwrap(),
            "{relative}"
        );
    }

    let store = Store::new(&target);
    let native = store.load_native_records().expect("native records survive");
    assert_eq!(native.len(), 1);
    assert_eq!(native[0].entry.name, "alpha");
    let upstream = store.load_upstream_records().expect("upstream records survive");
    assert_eq!(upstream.len(), 1);
    assert_eq!(upstream[0].acquired_at, 123);
    assert_eq!(upstream[0].raw_index_line, "{\"name\":\"serde\",\"vers\":\"1.0.0\"}");

    let second = layout.prepare(&environment).expect("migration is idempotent");
    assert_eq!(second.copied(), 0);
    assert_eq!(
        fs::read(target.join("pins/demo.json")).unwrap(),
        fs::read(legacy.join("pins/demo.json")).unwrap()
    );
}

#[test]
fn a_repeated_publish_is_unchanged_and_promotes_nothing() {
    let fake = FakeS3::start_verified();
    let work = scratch_root();
    write_config(work.path());
    let legacy = work.path().join("absent-legacy");
    let environment = environment(work.path(), &legacy, &fake);
    let manifest = fixture_manifest(work.path());

    let first = stuff::publish::run(publish(PACKAGE, &manifest), &environment)
        .expect("first cargo publish");
    assert_eq!(first.status, Status::Published);
    assert_eq!(first.packages, vec![format!("{PACKAGE}@0.1.0")]);
    let generation = generation_id(&work.path().join("cache/cargo/site"));

    let requests_before = fake.requests().len();
    let conditions_before = fake.put_conditions().len();
    let second = stuff::publish::run(publish(PACKAGE, &manifest), &environment)
        .expect("second cargo publish");
    assert_eq!(second.status, Status::Unchanged);
    assert_eq!(put_paths(&fake, requests_before), Vec::<String>::new());
    assert_eq!(fake.put_conditions().len(), conditions_before);
    assert_eq!(
        generation_id(&work.path().join("cache/cargo/site")),
        generation
    );
}

#[test]
fn a_cargo_dry_run_plans_from_the_real_live_and_local_state() {
    let fake = FakeS3::start_verified();
    let work = scratch_root();
    write_config(work.path());
    let legacy = work.path().join("absent-legacy");
    let environment = environment(work.path(), &legacy, &fake);

    let first = stuff::publish::run(
        publish(PACKAGE, &fixture_manifest(work.path())),
        &environment,
    )
    .expect("first cargo publish");
    assert_eq!(first.status, Status::Published);
    let state_dir = work.path().join("state/cargo");
    let site_dir = work.path().join("cache/cargo/site");
    let generation = generation_id(&site_dir);
    let state_before = tree(&state_dir);
    let site_before = tree(&site_dir);
    let remote_before = remote_snapshot(&fake);
    let conditions_before = fake.put_conditions().len();
    let requests_before = fake.requests().len();

    let manifest = second_crate(work.path());
    let outcome = stuff::publish::run(dry_run(SECOND_PACKAGE, &manifest), &environment)
        .expect("dry-run cargo publish");
    assert_eq!(outcome.status, Status::Published);

    assert_eq!(tree(&state_dir), state_before, "the dry run changed local state");
    assert_eq!(tree(&site_dir), site_before, "the dry run changed the live site");
    assert_eq!(remote_snapshot(&fake), remote_before, "the dry run changed the remote");
    assert_eq!(fake.put_conditions().len(), conditions_before);
    assert_eq!(mutation_count(&fake, requests_before), 0);
    assert_eq!(generation_id(&site_dir), generation);
    assert!(
        site_dir.join("generations").join(&generation).is_dir(),
        "the dry run dropped the existing generation"
    );
}

#[test]
fn the_live_snapshot_reads_the_current_generation_index_only() {
    let fake = FakeS3::start_verified();
    let work = scratch_root();
    write_config(work.path());
    let legacy = work.path().join("absent-legacy");
    let environment = environment(work.path(), &legacy, &fake);
    let manifest = fixture_manifest(work.path());

    stuff::publish::run(publish(PACKAGE, &manifest), &environment).expect("first publish");
    let generation = generation_id(&work.path().join("cache/cargo/site"));
    let requests_before = fake.requests().len();
    let outcome = stuff::publish::run(publish(PACKAGE, &manifest), &environment)
        .expect("republish");
    assert_eq!(outcome.status, Status::Unchanged);
    let fetched: Vec<String> = fake
        .requests()
        .into_iter()
        .skip(requests_before)
        .filter(|request| request.method == "GET")
        .map(|request| request.path)
        .filter(|path| path.starts_with(&format!("/stuff-mycrates/generations/{generation}/")))
        .collect();
    assert!(
        fetched.iter().any(|path| path.contains("/registry/")),
        "the snapshot did not pull the native index: {fetched:?}"
    );
    assert!(
        fetched.iter().all(|path| !path.contains("/blobs/")),
        "{fetched:?}"
    );
    let shard = stuff::publish::cargo::names::shard_for_name(&PACKAGE.to_lowercase())
        .expect("the fixture crate name shards");
    let cached = fs::read_to_string(
        work.path()
            .join("cache/cargo/stuff-mycrates/generations")
            .join(&generation)
            .join("registry")
            .join(shard),
    )
    .expect("read the pulled native index line");
    assert!(cached.contains(PACKAGE), "{cached}");
    assert!(cached.ends_with('\n'), "{cached}");
    let _: Value = deser_json::from_str(cached.trim()).expect("the pulled line is JSON");
}
