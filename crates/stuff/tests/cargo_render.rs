#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use deser_value::Value;
use stuff::publish::cargo::{
    AdoptSummary, CargoUrls, Publisher, RejectionKind, Store, adopt_site,
};

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../mycrates/tests/golden/adopt-publish")
}

fn scratch() -> tempfile::TempDir {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&base).expect("create TMPDIR");
    tempfile::Builder::new()
        .prefix("stuff-cargo-render.")
        .tempdir_in(&base)
        .expect("scratch dir under TMPDIR")
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create target directory");
    for entry in fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("source entry");
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let file_type = entry.file_type().expect("source entry type");
        if file_type.is_symlink() {
            let link = fs::read_link(&path).expect("read source symlink");
            std::os::unix::fs::symlink(link, &destination).expect("copy source symlink");
        } else if file_type.is_dir() {
            copy_tree(&path, &destination);
        } else {
            fs::copy(&path, &destination).expect("copy source file");
        }
    }
}

fn input_site(scratch: &Path) -> (PathBuf, PathBuf) {
    let source = scratch.join("input-site");
    copy_tree(&golden_root().join("input-site"), &source);
    (source, scratch.join("state"))
}

fn read_config(path: &Path) -> (String, bool) {
    let text = fs::read_to_string(path).expect("read config.json");
    let value: Value = deser_json::from_str(&text).expect("config.json is JSON");
    let map = value.as_map().expect("config.json is an object");
    let base = map
        .get("public_base_url")
        .and_then(|value| value.as_str())
        .expect("config.json has public_base_url")
        .to_string();
    let auth_required = map
        .get("auth_required")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    (base, auth_required)
}

fn tree_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_files(root, root, &mut files);
    files
}

fn collect_files(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(directory).expect("read generation directory") {
        let entry = entry.expect("generation entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("generation entry type");
        if file_type.is_dir() {
            collect_files(root, &path, files);
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("generation relative path")
                .to_path_buf();
            files.insert(relative, fs::read(&path).expect("read generation file"));
        }
    }
}

fn adopt(source: &Path, state: &Path) -> AdoptSummary {
    adopt_site(&Store::new(state), source).expect("adopt the input site")
}

fn blob_paths(source: &Path) -> Vec<PathBuf> {
    let mut blobs: Vec<PathBuf> = fs::read_dir(source.join("blobs"))
        .expect("read input blobs")
        .map(|entry| entry.expect("input blob").path())
        .collect();
    blobs.sort();
    blobs
}

#[test]
fn an_adopted_site_republishes_the_golden_generation_byte_for_byte() {
    let golden = golden_root();
    let scratch = scratch();
    let (source, state) = input_site(scratch.path());
    let site = scratch.path().join("site");
    let (public_base_url, auth_required) = read_config(&golden.join("config.json"));

    let summary = adopt(&source, &state);
    assert_eq!(
        (summary.native, summary.upstream, summary.blobs),
        (7, 3, 10)
    );

    let outcome = Publisher::new(&Store::new(&state), &site, CargoUrls::on_one_host(public_base_url.as_str()), auth_required)
        .publish()
        .expect("publish the adopted state");
    let expected_id = fs::read_to_string(golden.join("expected-generation-id"))
        .expect("read expected-generation-id")
        .trim()
        .to_string();
    assert_eq!(outcome.generation_id, expected_id);
    assert_eq!(
        fs::read_link(site.join("current")).expect("read current symlink"),
        PathBuf::from(format!("generations/{expected_id}"))
    );

    let expected_files = tree_files(&golden.join("expected-generation"));
    let produced_files = tree_files(&site.join("generations").join(&expected_id));
    assert_eq!(
        produced_files.keys().collect::<Vec<&PathBuf>>(),
        expected_files.keys().collect::<Vec<&PathBuf>>()
    );
    for (relative, expected_bytes) in &expected_files {
        assert_eq!(
            &produced_files[relative],
            expected_bytes,
            "{}",
            relative.display()
        );
    }
}

#[test]
fn adopting_the_same_site_twice_ingests_no_new_blobs() {
    let scratch = scratch();
    let (source, state) = input_site(scratch.path());
    let first = adopt(&source, &state);
    assert_eq!((first.native, first.upstream, first.blobs), (7, 3, 10));
    let second = adopt(&source, &state);
    assert_eq!((second.native, second.upstream, second.blobs), (7, 3, 0));
}

#[test]
fn a_tampered_blob_is_an_integrity_error() {
    let scratch = scratch();
    let (source, state) = input_site(scratch.path());
    let blobs = blob_paths(&source);
    let victim = &blobs[0];
    let mut bytes = fs::read(victim).expect("read blob");
    bytes.extend_from_slice(b"tampered");
    fs::write(victim, bytes).expect("tamper blob");

    let error = adopt_site(&Store::new(&state), &source).expect_err("tampered blob must fail");
    assert_eq!(error.kind, RejectionKind::Integrity);
    assert!(
        error.detail.contains("not the index checksum"),
        "{}",
        error.detail
    );
}

#[test]
fn a_missing_blob_is_an_integrity_error() {
    let scratch = scratch();
    let (source, state) = input_site(scratch.path());
    fs::remove_file(&blob_paths(&source)[0]).expect("remove blob");

    let error = adopt_site(&Store::new(&state), &source).expect_err("missing blob must fail");
    assert_eq!(error.kind, RejectionKind::Integrity);
    assert!(error.detail.contains("missing blob"), "{}", error.detail);
}
