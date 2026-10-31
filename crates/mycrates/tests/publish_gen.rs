use mycrates::model::IndexEntry;
use mycrates::publish::Publisher;
use mycrates::store::{NativeRecord, Store};
use stuff_store::Site;
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use tempfile::tempdir;

fn sample_entry(name: &str, vers: &str, cksum: &str) -> IndexEntry {
    IndexEntry {
        name: name.to_string(),
        vers: vers.to_string(),
        deps: vec![],
        cksum: cksum.to_string(),
        features: BTreeMap::new(),
        yanked: false,
        links: None,
        rust_version: None,
        v: Some(1),
        extra: BTreeMap::new(),
    }
}

#[test]
fn shared_state_keeps_both_publishers_packages() {
    let dir = tempdir().unwrap();
    let state = dir.path().join("state");
    let site = dir.path().join("site");

    for (name, bytes) in [("from-a", b"a-bytes".as_slice()), ("from-b", b"b-bytes".as_slice())] {
        let store = Store::new(state.clone());
        let sha = store.ingest_blob(Cursor::new(bytes)).unwrap();
        store
            .put_native_record(&NativeRecord {
                name: name.to_string(),
                vers: "1.0.0".to_string(),
                checksum: sha.clone(),
                yanked: false,
                entry: sample_entry(name, "1.0.0", &sha),
            })
            .unwrap();
        Publisher::new(&store, site.clone(), "http://localhost:8080".to_string())
            .publish()
            .unwrap();
    }

    let current = site.join("current/catalog.json");
    let catalog = stuff_catalog::parse_catalog_json(&std::fs::read_to_string(current).unwrap()).unwrap();
    assert!(catalog.find("from-a", Some(stuff_catalog::CargoSource::Native)).is_some());
    assert!(catalog.find("from-b", Some(stuff_catalog::CargoSource::Native)).is_some());
    for name in ["from-a", "from-b"] {
        let shard = mycrates::index::shard_for_name(name);
        assert!(site.join("current/registry").join(shard).is_file(), "missing index for {name}");
    }
    let blobs = site.join("blobs");
    assert_eq!(std::fs::read_dir(blobs).unwrap().count(), 2);
}

#[test]
fn generation_determinism_and_noop() {
    let dir1 = tempdir().unwrap();
    let store1 = Store::new(dir1.path().join("state"));
    let site1 = dir1.path().join("site");

    let dir2 = tempdir().unwrap();
    let store2 = Store::new(dir2.path().join("state"));
    let site2 = dir2.path().join("site");

    let data = b"sample-crate-tarball-bytes";
    let sha = store1.ingest_blob(Cursor::new(data)).unwrap();
    let sha2 = store2.ingest_blob(Cursor::new(data)).unwrap();
    assert_eq!(sha, sha2);

    let rec = NativeRecord {
        name: "foo".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        yanked: false,
        entry: sample_entry("foo", "1.0.0", &sha),
    };
    store1.put_native_record(&rec).unwrap();
    store2.put_native_record(&rec).unwrap();

    let pub1 = Publisher::new(&store1, site1.clone(), "http://localhost:8080".to_string());
    let res1 = pub1.publish().unwrap();
    assert!(!res1.is_noop);

    let pub2 = Publisher::new(&store2, site2.clone(), "http://localhost:8080".to_string());
    let res2 = pub2.publish().unwrap();
    assert!(!res2.is_noop);

    // Identical generation ID across independent state and site dirs
    assert_eq!(res1.generation_id, res2.generation_id);

    // Second publish on site1 detects no-op
    let res1_again = pub1.publish().unwrap();
    assert!(res1_again.is_noop);
    assert_eq!(res1_again.generation_id, res1.generation_id);

    // Verify current symlink
    let current_target = fs::read_link(site1.join("current")).unwrap();
    assert_eq!(
        current_target.to_string_lossy(),
        format!("generations/{}", res1.generation_id)
    );
}

#[test]
fn missing_blob_rejected_before_switch() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");

    store.ensure_dirs().unwrap();
    let rec = NativeRecord {
        name: "bar".to_string(),
        vers: "0.1.0".to_string(),
        checksum: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
        yanked: false,
        entry: sample_entry("bar", "0.1.0", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
    };
    store.put_native_record(&rec).unwrap();

    // Publisher must fail because blob does not exist in store
    let publ = Publisher::new(&store, site.clone(), "http://localhost:8080".to_string());
    let err = publ.publish().unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("missing blob"));
    assert!(!site.join("current").exists());
}

#[test]
fn root_and_prefix_hosting_config() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");

    let data = b"payload";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();
    let rec = NativeRecord {
        name: "baz".to_string(),
        vers: "1.2.3".to_string(),
        checksum: sha.clone(),
        yanked: false,
        entry: sample_entry("baz", "1.2.3", &sha),
    };
    store.put_native_record(&rec).unwrap();

    // Test with prefix
    let publ = Publisher::new(
        &store,
        site.clone(),
        "https://deps.example.net/mycrates".to_string(),
    );
    let res = publ.publish().unwrap();

    let gen_dir = site.join("generations").join(&res.generation_id);
    let native_cfg: deser_value::Value =
        deser_json::from_str(&fs::read_to_string(gen_dir.join("registry/config.json")).unwrap()).unwrap();
    assert_eq!(
        native_cfg["dl"].as_str(),
        Some("https://deps.example.net/mycrates/blobs/{sha256-checksum}.crate")
    );
    assert_eq!(native_cfg["api"].as_str(), Some("https://deps.example.net/mycrates"));

    let mirror_cfg: deser_value::Value =
        deser_json::from_str(&fs::read_to_string(gen_dir.join("crates-io/config.json")).unwrap()).unwrap();
    assert_eq!(
        mirror_cfg["dl"].as_str(),
        Some("https://deps.example.net/mycrates/blobs/{sha256-checksum}.crate")
    );
    assert!(mirror_cfg.get("api").is_none());
}

#[test]
fn tampered_blob_rejected_before_switch() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");

    let data = b"original-content";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();
    let rec = NativeRecord {
        name: "tamper-pkg".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        yanked: false,
        entry: sample_entry("tamper-pkg", "1.0.0", &sha),
    };
    store.put_native_record(&rec).unwrap();

    // Tamper with the blob in store
    let blob_path = store.blobs_dir().join(format!("{}.crate", sha));
    fs::write(&blob_path, b"modified-bytes").unwrap();

    let publ = Publisher::new(&store, site.clone(), "http://localhost:8080".to_string());
    let err = publ.publish().unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("tampered blob"));
    assert!(!site.join("current").exists());
}

#[test]
fn damaged_generation_requires_explicit_repair() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");

    let data = b"repair-payload";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();
    let rec = NativeRecord {
        name: "repair-pkg".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        yanked: false,
        entry: sample_entry("repair-pkg", "1.0.0", &sha),
    };
    store.put_native_record(&rec).unwrap();

    let publ = Publisher::new(&store, site.clone(), "http://localhost:8080".to_string());
    let res = publ.publish().unwrap();
    assert!(!res.is_noop);

    let gen_dir = site.join("generations").join(&res.generation_id);
    let shard = mycrates::index::shard_for_name("repair-pkg");
    let index_file = gen_dir.join("registry").join(&shard);
    assert!(index_file.exists());

    // 1. Delete the index file: republish must not rewrite an immutable
    // generation; explicit removal is the repair path.
    fs::remove_file(&index_file).unwrap();
    let err = publ.publish().unwrap_err();
    assert!(format!("{err}").contains("refusing to replace an immutable generation"));
    assert!(!index_file.exists(), "damaged generation must be left for explicit repair");
    fs::remove_dir_all(&gen_dir).unwrap();
    let res2 = publ.publish().unwrap();
    assert!(!res2.is_noop, "removed generation must be regenerated");
    assert!(index_file.exists());

    // 2. Replace a served blob: republish must fail, not serve the bad copy.
    let blob_path = site.join("blobs").join(format!("{}.crate", sha));
    assert!(blob_path.exists());
    fs::write(&blob_path, b"replaced-by-attacker").unwrap();
    let err = publ.publish().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("served blob mismatch") || msg.contains("tampered blob"),
        "unexpected error: {msg}"
    );

    // 3. Dangling current link: republish must repair to a valid generation.
    let current = site.join("current");
    let _ = fs::remove_file(&current);
    std::os::unix::fs::symlink("generations/nonexistent_gen_repair", &current).unwrap();
    fs::write(&blob_path, data).unwrap();
    let res3 = publ.publish().unwrap();
    assert!(!res3.is_noop, "dangling current must be repaired");
    let target = fs::read_link(&current).unwrap();
    assert!(site.join(&target).exists());
}

#[test]
fn staging_recovery() {
    let dir = tempdir().unwrap();
    let site = dir.path().join("site");

    fs::create_dir_all(&site).unwrap();
    let stale_staging = site.join(".staging.interrupted.123");
    fs::create_dir_all(&stale_staging).unwrap();
    let stale_tmp = site.join(".current.tmp.interrupted.456");
    fs::write(&stale_tmp, b"temp").unwrap();

    Site::new(site.clone()).recover_staging().unwrap();

    assert!(!stale_staging.exists());
    assert!(!stale_tmp.exists());
}

#[test]
fn interrupted_temp_symlink_does_not_block_recovery() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");
    fs::create_dir_all(&site).unwrap();
    store.ensure_dirs().unwrap();

    let tmp_link = site.join(".current.tmp.1.2.3");
    std::os::unix::fs::symlink("generations/ghost", &tmp_link).unwrap();
    assert!(tmp_link.is_symlink());

    let publ = Publisher::new(&store, site.clone(), "http://localhost:8080".to_string());
    let res = publ.publish().unwrap();
    assert!(!tmp_link.exists() && !tmp_link.is_symlink(), "owned temp symlink must be cleaned");
    let current = site.join("current");
    assert!(current.is_symlink());
    assert_eq!(fs::read_link(&current).unwrap().to_string_lossy(), format!("generations/{}", res.generation_id));
}

#[test]
fn temp_symlink_pointing_to_dir_is_unlinked_not_traversed() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");
    fs::create_dir_all(&site).unwrap();
    store.ensure_dirs().unwrap();

    let gen_dir = site.join("generations/keepme");
    fs::create_dir_all(&gen_dir).unwrap();
    fs::write(gen_dir.join("config.json"), b"keep").unwrap();
    let tmp_link = site.join(".current.tmp.9.9.9");
    std::os::unix::fs::symlink("generations/keepme", &tmp_link).unwrap();

    Site::new(site.clone()).recover_staging().unwrap();
    assert!(!tmp_link.is_symlink());
    assert!(gen_dir.exists(), "recover must unlink the symlink, not delete its target");
    assert!(gen_dir.join("config.json").exists());
}

#[test]
fn large_blob_publishes_via_streaming() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");
    let big: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
    let sha = store.ingest_blob(Cursor::new(&big)).unwrap();
    let rec = NativeRecord {
        name: "big-pkg".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        yanked: false,
        entry: sample_entry("big-pkg", "1.0.0", &sha),
    };
    store.put_native_record(&rec).unwrap();
    let publ = Publisher::new(&store, site.clone(), "http://localhost:8080".to_string());
    let res = publ.publish().unwrap();
    assert!(!res.is_noop);
    let served = fs::read(site.join("blobs").join(format!("{}.crate", sha))).unwrap();
    assert_eq!(served.len(), big.len());
}

#[test]
fn interrupted_upload_leaves_no_permanent_partial() {    use mycrates::wire::decode_publish_payload;
    let dir = tempdir().unwrap();
    let staging = dir.path().join("blobs");
    fs::create_dir_all(&staging).unwrap();
    let json = deser_json::to_string(&deser_value::value!({"name":"x","vers":"1.0.0","deps":[],"features":{}})).unwrap();
    let mut payload = Vec::new();
    payload.extend_from_slice(&(json.len() as u32).to_le_bytes());
    payload.extend_from_slice(json.as_bytes());
    payload.extend_from_slice(&100u32.to_le_bytes());
    payload.extend_from_slice(&[0u8; 10]);
    struct Truncated(Vec<u8>);
    impl std::io::Read for Truncated {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "disconnect"));
            }
            let n = buf.len().min(self.0.len()).min(3);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0.drain(..n);
            Ok(n)
        }
    }
    let content_len = (4 + json.len() + 4 + 100) as u64;
    let reader = Truncated(payload);
    let err = decode_publish_payload(reader, content_len, 10 * 1024 * 1024, &staging).unwrap_err();
    assert!(format!("{err}").contains("failed to read crate bytes"));
    let leftovers: Vec<_> = fs::read_dir(&staging).unwrap().collect();
    assert!(leftovers.is_empty(), "partial upload must be cleaned, found {leftovers:?}");
    let blobs_store = Store::new(dir.path().join("state3"));
    blobs_store.ensure_dirs().unwrap();
    let bdir = blobs_store.blobs_dir();
    fs::write(bdir.join(".upload.orphan.999"), b"partial").unwrap();
    blobs_store.recover_staging().unwrap();
    assert!(!bdir.join(".upload.orphan.999").exists());
}

#[test]
fn catalog_tracks_publish_and_rollback() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("state"));
    let site = dir.path().join("site");

    let data = b"catalog-acceptance-tarball";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();
    let rec = NativeRecord {
        name: "widget".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        yanked: false,
        entry: sample_entry("widget", "1.0.0", &sha),
    };
    store.put_native_record(&rec).unwrap();

    let publ = Publisher::new(&store, site.clone(), "http://localhost:8080".to_string());
    let first = publ.publish().unwrap();
    assert!(!first.is_noop);

    // New version is visible in the published catalog and landing page.
    let gen1 = site.join("generations").join(&first.generation_id);
    let catalog_text = fs::read_to_string(gen1.join("catalog.json")).unwrap();
    let catalog = stuff_catalog::parse_catalog_json(&catalog_text).unwrap();
    assert_eq!(catalog.generation, first.generation_id);
    let pkg = catalog.find("widget", Some(stuff_catalog::CargoSource::Native)).expect("widget in catalog");
    assert_eq!(pkg.latest, "1.0.0");
    assert!(!pkg.versions[0].yanked);
    let landing = fs::read_to_string(gen1.join("index.html")).unwrap();
    assert!(landing.contains("widget") && landing.contains("catalog.json"));

    // Yanking advances the catalog; the old generation is byte-identical.
    store.set_native_yank("widget", "1.0.0", true).unwrap();
    let second = publ.publish().unwrap();
    assert!(!second.is_noop);
    assert_ne!(second.generation_id, first.generation_id);
    let gen2 = site.join("generations").join(&second.generation_id);
    let yanked = stuff_catalog::parse_catalog_json(
        &fs::read_to_string(gen2.join("catalog.json")).unwrap(),
    )
    .unwrap();
    assert!(yanked.find("widget", Some(stuff_catalog::CargoSource::Native)).unwrap().versions[0].yanked);
    assert_eq!(
        fs::read(gen1.join("catalog.json")).unwrap(),
        catalog_text.as_bytes(),
        "old generation must be immutable"
    );

    // Rolling back to the first generation restores its catalog view.
    let site_handle = Site::new(site.clone());
    let staged = site_handle.stage().unwrap();
    let staged_path = staged.into_path();
    site_handle
        .activate(
            staged_path,
            &first.generation_id,
            &|gen_dir| Ok(gen_dir.join("catalog.json").is_file()),
        )
        .unwrap();
    assert_eq!(
        fs::read_link(site.join("current")).unwrap().to_string_lossy(),
        format!("generations/{}", first.generation_id)
    );
    let restored = stuff_catalog::parse_catalog_json(
        &fs::read_to_string(site.join("current").join("catalog.json")).unwrap(),
    )
    .unwrap();
    assert!(!restored.find("widget", Some(stuff_catalog::CargoSource::Native)).unwrap().versions[0].yanked);
}
