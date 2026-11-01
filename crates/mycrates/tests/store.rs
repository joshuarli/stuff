use mycrates::error::Error;
use mycrates::model::{IndexEntry, Version};
use mycrates::publish::Publisher;
use mycrates::store::{NativeRecord, Store};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write};
use std::thread;
use std::time::{Duration, SystemTime};
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
fn ingest_same_bytes_and_conflict() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let data1 = b"crate-archive-bytes-version-1";
    let sha1 = store.ingest_blob(Cursor::new(data1)).unwrap();
    assert_eq!(sha1, "9701dd578758d425fc4a09b80c796117e5bbeb3ce77e9e2c98be555006263453");

    let rec1 = NativeRecord {
        entry: sample_entry("my-pkg", "1.0.0", &sha1),
    };
    let written = store.put_native_record(&rec1).unwrap();
    assert!(written);

    // Ingest same bytes again is a no-op
    let sha1_again = store.ingest_blob(Cursor::new(data1)).unwrap();
    assert_eq!(sha1_again, sha1);
    let written_again = store.put_native_record(&rec1).unwrap();
    assert!(!written_again);

    // Ingest different bytes for same crate and version is a conflict
    let data2 = b"different-bytes-for-same-version";
    let sha2 = store.ingest_blob(Cursor::new(data2)).unwrap();
    let rec2 = NativeRecord {
        entry: sample_entry("my-pkg", "1.0.0", &sha2),
    };
    let err = store.put_native_record(&rec2).unwrap_err();
    match err {
        Error::Conflict(_) => {}
        other => panic!("expected Conflict error, got {other:?}"),
    }

    // Existing record was not overwritten
    let records = store.load_native_records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].entry.cksum, sha1);
}

#[test]
fn name_collision_detection() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let data = b"some-crate-data";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();

    let rec1 = NativeRecord {
        entry: sample_entry("foo_bar", "1.0.0", &sha),
    };
    store.put_native_record(&rec1).unwrap();

    // Trying to publish "foo-bar" when "foo_bar" exists must fail with Conflict
    let rec2 = NativeRecord {
        entry: sample_entry("foo-bar", "1.0.0", &sha),
    };
    let err = store.put_native_record(&rec2).unwrap_err();
    match err {
        Error::Conflict(msg) => {
            assert!(msg.contains("collision"));
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn build_metadata_accepted_with_uniqueness_key() {
    assert!(Version::new("1.0.0+build1").is_ok());
    assert!(Version::new("2.0.16+zstd.1.5.7").is_ok());
    let v = Version::new("2.0.16+zstd.1.5.7").unwrap();
    assert_eq!(v.uniqueness_key(), "2.0.16");
    assert_eq!(v.as_str(), "2.0.16+zstd.1.5.7");
    assert!(Version::new("1.0.0").is_ok());
}

#[test]
fn native_build_metadata_identity_conflict() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let rec1 = NativeRecord {
        entry: sample_entry("meta-pkg", "1.0.7", "sha_a"),
    };
    assert!(store.put_native_record(&rec1).unwrap());

    let rec2 = NativeRecord {
        entry: sample_entry("meta-pkg", "1.0.7+extra", "sha_b"),
    };
    let err = store.put_native_record(&rec2).unwrap_err();
    match err {
        Error::Conflict(msg) => {
            assert!(msg.contains("conflicts with existing"));
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn verify_detects_corrupted_blob() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let data = b"valid-blob-contents";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();
    let rec = NativeRecord {
        entry: sample_entry("test-pkg", "0.1.0", &sha),
    };
    store.put_native_record(&rec).unwrap();

    // Verification succeeds initially
    store.verify().unwrap();

    // Corrupt the blob on disk
    let blob_path = store.blobs_dir().join(format!("{}.crate", sha));
    fs::write(&blob_path, b"tampered-contents").unwrap();

    // Verify must now fail naming the file
    let err = store.verify().unwrap_err();
    match err {
        Error::Integrity(msg) => {
            assert!(msg.contains("corrupted blob"));
            assert!(msg.contains(&sha));
        }
        other => panic!("expected Integrity error, got {other:?}"),
    }
}

#[test]
fn store_exclusive_locking() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let guard1 = store.lock_exclusive().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiting = store.clone();
    let waiter = thread::spawn(move || {
        let guard = waiting.lock_exclusive().unwrap();
        let _ = tx.send(());
        guard
    });
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

    drop(guard1);
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the store lock was not released");
    drop(waiter.join().unwrap());
}

fn age_file(path: &std::path::Path) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(7200))
        .unwrap();
}

#[test]
fn stale_temp_cleanup() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());
    store.ensure_dirs().unwrap();

    let tmp_file = store.blobs_dir().join(".tmp.stale.crate");
    fs::write(&tmp_file, b"stale data").unwrap();

    store.recover_staging().unwrap();
    assert!(!tmp_file.exists());
}

#[test]
fn stale_uploads_are_reclaimed_only_by_age() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());
    store.ensure_dirs().unwrap();

    let fresh = store.uploads_dir().join(".upload.123.1.1");
    fs::write(&fresh, b"in-flight").unwrap();
    let stale = store.uploads_dir().join(".upload.456.1.2");
    fs::write(&stale, b"abandoned").unwrap();
    age_file(&stale);

    store.recover_uploads().unwrap();
    assert!(fresh.exists());
    assert!(!stale.exists());
}

#[test]
fn concurrent_verify_and_publish_leave_in_flight_uploads_alone() {
    let dir = tempdir().unwrap();
    let state = dir.path().join("state");
    let site = dir.path().join("site");
    let store = Store::new(state);
    store.ensure_dirs().unwrap();

    let upload = store.uploads_dir().join(".upload.424242.1.1");
    let mut writer = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&upload)
        .unwrap();
    writer.write_all(b"in-flight").unwrap();
    writer.flush().unwrap();

    let verify_store = store.clone();
    let verify = thread::spawn(move || verify_store.verify());
    let publish_store = store.clone();
    let publish_site = site.clone();
    let publish = thread::spawn(move || {
        Publisher::new(
            &publish_store,
            publish_site,
            "http://localhost:8080".to_string(),
        )
        .publish()
    });

    verify.join().unwrap().unwrap();
    publish.join().unwrap().unwrap();

    writer.write_all(b"-more").unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    assert_eq!(fs::read(&upload).unwrap(), b"in-flight-more");
}

#[test]
fn mixed_case_lookup_and_collision() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let data = b"case-data";
    let sha = store.ingest_blob(Cursor::new(data)).unwrap();
    let rec = NativeRecord {
        entry: sample_entry("Foo_Bar", "1.0.0", &sha),
    };
    store.put_native_record(&rec).unwrap();

    let dup = NativeRecord {
        entry: sample_entry("foo-bar", "2.0.0", &sha),
    };
    let err = store.put_native_record(&dup).unwrap_err();
    match err {
        Error::Conflict(msg) => assert!(msg.contains("collision")),
        other => panic!("expected Conflict, got {other:?}"),
    }

    let recs = store.load_native_records().unwrap();
    assert!(recs.iter().any(|r| r.entry.name == "Foo_Bar"));

    use mycrates::index::shard_for_name;
    assert_eq!(shard_for_name(&"Foo_Bar".to_lowercase()), shard_for_name("foo_bar"));
}

#[test]
fn successive_mixed_case_releases_succeed() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());
    let sha1 = store.ingest_blob(std::io::Cursor::new(b"mixed-1")).unwrap();
    let sha2 = store.ingest_blob(std::io::Cursor::new(b"mixed-2")).unwrap();
    let rec1 = NativeRecord {
        entry: sample_entry("MyCrate", "1.0.0", &sha1),
    };
    assert!(store.put_native_record(&rec1).unwrap());
    let rec2 = NativeRecord {
        entry: sample_entry("MyCrate", "1.1.0", &sha2),
    };
    assert!(store.put_native_record(&rec2).unwrap());
    let recs = store.load_native_records().unwrap();
    assert_eq!(recs.len(), 2);
}

#[test]
fn overlapping_normalized_names_across_registries_allowed() {
    use mycrates::store::UpstreamRecord;
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());
    let sha = store.ingest_blob(std::io::Cursor::new(b"cross")).unwrap();
    let native = NativeRecord {
        entry: sample_entry("foo_bar", "1.0.0", &sha),
    };
    store.put_native_record(&native).unwrap();
    let upstream = UpstreamRecord {
        name: "foo-bar".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        raw_index_line: deser_json::to_string(&deser_value::value!({"name":"foo-bar","vers":"1.0.0","deps":[],"cksum":sha,"features":{},"yanked":false})).unwrap(),
        acquired_at: 1,
    };
    assert!(store.put_upstream_record(&upstream).unwrap());
    assert_eq!(store.load_native_records().unwrap().len(), 1);
    assert_eq!(store.load_upstream_records().unwrap().len(), 1);
}

#[test]
fn prerelease_version_distinct_from_release() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());

    let rec_pre = NativeRecord {
        entry: sample_entry("my-pkg", "1.0.0-alpha.1", "sha_pre"),
    };
    let rec_rel = NativeRecord {
        entry: sample_entry("my-pkg", "1.0.0", "sha_rel"),
    };

    assert!(store.put_native_record(&rec_pre).unwrap());
    assert!(store.put_native_record(&rec_rel).unwrap());

    let records = store.load_native_records().unwrap();
    assert_eq!(records.len(), 2);
}

#[test]
fn native_record_disagreement_fails_load() {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().to_path_buf());
    let sha = "a".repeat(64);
    let path = store.native_dir().join("agree-pkg").join("1.0.0.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "name = \"other-pkg\"\nvers = \"1.0.0\"\nchecksum = \"{sha}\"\nyanked = false\n\n[entry]\nname = \"agree-pkg\"\nvers = \"1.0.0\"\ncksum = \"{sha}\"\nyanked = false\n"
        ),
    )
    .unwrap();

    let err = store.load_native_records().unwrap_err();
    match err {
        Error::Integrity(msg) => assert!(msg.contains("disagrees"), "{msg}"),
        other => panic!("expected Integrity, got {other:?}"),
    }
}
