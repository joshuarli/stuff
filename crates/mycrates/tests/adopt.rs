use mycrates::adopt::{adopt_site, AdoptSummary};
use mycrates::error::Error;
use mycrates::model::IndexEntry;
use mycrates::publish::Publisher;
use mycrates::store::{NativeRecord, Store, UpstreamRecord};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

const BASE_URL: &str = "https://crates.example.test";

fn entry(name: &str, vers: &str, cksum: &str, yanked: bool) -> IndexEntry {
    IndexEntry {
        name: name.to_string(),
        vers: vers.to_string(),
        deps: vec![],
        cksum: cksum.to_string(),
        features: BTreeMap::new(),
        yanked,
        links: None,
        rust_version: Some("1.85".to_string()),
        v: Some(2),
        extra: BTreeMap::new(),
    }
}

fn publish_fixture(root: &Path) -> (PathBuf, String) {
    let store = Store::new(root.join("state"));
    let site = root.join("site");
    for (name, vers, bytes, yanked) in [
        ("alpha", "1.0.0", b"alpha-1".as_slice(), false),
        ("alpha", "1.1.0", b"alpha-2".as_slice(), true),
        ("Beta_Tool", "0.3.0", b"beta".as_slice(), false),
    ] {
        let sha = store.ingest_blob(Cursor::new(bytes)).unwrap();
        store
            .put_native_record(&NativeRecord {
                name: name.to_string(),
                vers: vers.to_string(),
                checksum: sha.clone(),
                yanked,
                entry: entry(name, vers, &sha, yanked),
            })
            .unwrap();
    }
    let sha = store.ingest_blob(Cursor::new(b"serde".as_slice())).unwrap();
    let raw_index_line = mycrates::index::serialize_index_line(&entry("serde", "1.0.0", &sha, false)).unwrap();
    store
        .put_upstream_record(&UpstreamRecord {
            name: "serde".to_string(),
            vers: "1.0.0".to_string(),
            checksum: sha,
            raw_index_line,
            acquired_at: 1,
        })
        .unwrap();
    let generation = Publisher::new(&store, site.clone(), BASE_URL.to_string())
        .publish()
        .unwrap()
        .generation_id;
    (site, generation)
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for item in fs::read_dir(&dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let key = path.strip_prefix(root).unwrap().to_string_lossy().to_string();
                files.insert(key, fs::read(&path).unwrap());
            }
        }
    }
    files
}

#[test]
fn adopted_state_republishes_the_identical_generation() {
    let live = tempdir().unwrap();
    let (live_site, live_generation) = publish_fixture(live.path());

    let restored = tempdir().unwrap();
    let store = Store::new(restored.path().join("state"));
    let summary = adopt_site(&store, &live_site).unwrap();
    assert_eq!(summary, AdoptSummary { native: 3, upstream: 1, blobs: 4 });

    let site = restored.path().join("site");
    let result = Publisher::new(&store, site.clone(), BASE_URL.to_string()).publish().unwrap();
    assert_eq!(result.generation_id, live_generation);
    assert_eq!(
        tree(&site.join("generations").join(&live_generation)),
        tree(&live_site.join("generations").join(&live_generation))
    );
    assert_eq!(tree(&site.join("blobs")), tree(&live_site.join("blobs")));

    let again = adopt_site(&store, &live_site).unwrap();
    assert_eq!(again, AdoptSummary { native: 3, upstream: 1, blobs: 0 });
}

#[test]
fn adopting_a_tampered_blob_is_an_integrity_error() {
    let live = tempdir().unwrap();
    let (live_site, _) = publish_fixture(live.path());
    let blob = fs::read_dir(live_site.join("blobs")).unwrap().next().unwrap().unwrap().path();
    let mut file = fs::OpenOptions::new().write(true).open(&blob).unwrap();
    std::io::Write::write_all(&mut file, b"tampered").unwrap();
    drop(file);

    let restored = tempdir().unwrap();
    let store = Store::new(restored.path().join("state"));
    match adopt_site(&store, &live_site).unwrap_err() {
        Error::Integrity(_) => {}
        other => panic!("expected an integrity error, got {other}"),
    }
}

#[test]
fn adopting_a_site_without_a_blob_fails() {
    let live = tempdir().unwrap();
    let (live_site, _) = publish_fixture(live.path());
    let blob = fs::read_dir(live_site.join("blobs")).unwrap().next().unwrap().unwrap().path();
    fs::remove_file(blob).unwrap();

    let restored = tempdir().unwrap();
    let store = Store::new(restored.path().join("state"));
    assert!(matches!(adopt_site(&store, &live_site).unwrap_err(), Error::Integrity(_)));
}
