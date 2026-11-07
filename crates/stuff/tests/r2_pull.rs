#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use stuff::publish::config::Config;
use stuff::publish::r2::pull::{
    download, list_all, md5_hex, multipart_etag, pull, verify_manifest, PullOptions,
};
use stuff::publish::r2::{Credentials, ListedObject, R2Client};
use stuff::publish::snapshot::{metadata_bucket, shared_stores, Snapshot};
use stuff::publish::Registry;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

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

fn object_entry(key: &str, size: u64, etag: &str) -> String {
    format!(r#"{{"key":"{key}","size":{size},"etag":"{etag}"}}"#)
}

fn write_manifest(root: &Path, objects: &[String]) {
    fs::create_dir_all(root.join("manifests")).unwrap();
    let document = format!(
        r#"{{"schemaVersion":1,"objects":[{}]}}"#,
        objects.join(",")
    );
    fs::write(root.join("manifests").join("g1.json"), document).unwrap();
}

fn seed_big(root: &Path) {
    fs::create_dir_all(root.join("blobs")).unwrap();
    fs::write(root.join("blobs").join("big"), b"abcdefgh").unwrap();
}

fn object_get_paths(fake: &FakeS3) -> Vec<String> {
    let mut paths: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|request| request.method == "GET")
        .map(|request| request.path)
        .filter(|path| path.matches('/').count() > 1)
        .collect();
    paths.sort();
    paths
}

#[test]
fn multipart_etag_matches_the_manifest() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    seed_big(root);
    let etag = multipart_etag(b"abcdefgh", 4);
    assert!(etag.ends_with("-2"), "{etag}");
    write_manifest(root, &[object_entry("blobs/big", 8, &etag)]);
    assert_eq!(verify_manifest(root, "g1", 4).unwrap(), 1);
}

#[test]
fn missing_object_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    seed_big(root);
    let etag = multipart_etag(b"abcdefgh", 4);
    write_manifest(root, &[object_entry("blobs/missing", 8, &etag)]);
    let error = verify_manifest(root, "g1", 4).unwrap_err();
    assert_eq!(error.exit_code(), 4, "{error}");
    assert!(error.to_string().contains("was not pulled"), "{error}");
}

#[test]
fn size_mismatch_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    seed_big(root);
    let etag = multipart_etag(b"abcdefgh", 4);
    write_manifest(root, &[object_entry("blobs/big", 7, &etag)]);
    let error = verify_manifest(root, "g1", 4).unwrap_err();
    assert_eq!(error.exit_code(), 4, "{error}");
    assert!(
        error.to_string().contains("bytes, but its manifest records 7"),
        "{error}"
    );
}

#[test]
fn etag_mismatch_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    seed_big(root);
    let bad = format!("{}-2", "0".repeat(32));
    write_manifest(root, &[object_entry("blobs/big", 8, &bad)]);
    let error = verify_manifest(root, "g1", 4).unwrap_err();
    assert_eq!(error.exit_code(), 4, "{error}");
    assert!(
        error.to_string().contains("differs from its manifest ETag"),
        "{error}"
    );
}

#[test]
fn generation_without_a_manifest_is_not_verified() {
    let work = tempfile::tempdir().unwrap();
    assert_eq!(verify_manifest(work.path(), "g1", 4).unwrap(), 0);
}

#[test]
fn multipart_object_is_verified_with_the_multipart_etag() {
    let fake = FakeS3::start_verified();
    let data = b"abcdefgh";
    fake.seed("bucket", "blobs/object", data, "application/octet-stream");
    let work = tempfile::tempdir().unwrap();
    let object = ListedObject {
        key: "blobs/object".to_string(),
        size: data.len() as u64,
        etag: multipart_etag(data, 4),
    };
    download(&client(&fake), "bucket", work.path(), &object, 4).unwrap();
    assert_eq!(fs::read(work.path().join("blobs/object")).unwrap(), data);
}

#[test]
fn single_part_object_still_uses_md5() {
    let fake = FakeS3::start_verified();
    let data = b"single";
    fake.seed("bucket", "blobs/object", data, "application/octet-stream");
    let work = tempfile::tempdir().unwrap();
    let object = ListedObject {
        key: "blobs/object".to_string(),
        size: data.len() as u64,
        etag: md5_hex(data),
    };
    download(&client(&fake), "bucket", work.path(), &object, 4).unwrap();
    assert_eq!(fs::read(work.path().join("blobs/object")).unwrap(), data);
}

#[test]
fn corrupt_download_is_rejected() {
    let fake = FakeS3::start_verified();
    let data = b"abcdefgh";
    fake.seed("bucket", "blobs/object", data, "application/octet-stream");
    let work = tempfile::tempdir().unwrap();
    let object = ListedObject {
        key: "blobs/object".to_string(),
        size: data.len() as u64,
        etag: format!("{}-2", "0".repeat(32)),
    };
    let error = download(&client(&fake), "bucket", work.path(), &object, 4).unwrap_err();
    assert_eq!(error.exit_code(), 4, "{error}");
    assert!(error.to_string().contains("could not pull blobs/object"), "{error}");
}

#[test]
fn listing_uses_paginated_s3() {
    let fake = FakeS3::start_verified();
    let keys = ["generations/g/a", "generations/g/b", "generations/g/c"];
    for key in keys {
        fake.seed("bucket", key, key.as_bytes(), "text/plain");
    }
    let listed = list_all(&client(&fake).with_list_page(2), "bucket").unwrap();
    assert_eq!(
        listed.iter().map(|object| object.key.as_str()).collect::<Vec<_>>(),
        keys
    );
    assert_eq!(listed[0].size, keys[0].len() as u64);
    assert_eq!(listed[0].etag, md5_hex(keys[0].as_bytes()));
}

#[test]
fn follows_the_backend_listing_order_across_pages() {
    const BACKEND_ORDER: [&str; 5] = [
        "generations/g/crates-io/ts/-r/ts-rs-macros",
        "generations/g/crates-io/ts/-r/ts-rs",
        "generations/g/crates-io/tu/rb/turboshake",
        "generations/g/crates-io/tw/ow/twoway",
        "generations/g/crates-io/ty/pe/type-map",
    ];
    let fake = FakeS3::start_verified();
    for key in BACKEND_ORDER {
        fake.seed("bucket", key, key.as_bytes(), "text/plain");
    }
    let listed = list_all(&client(&fake).with_list_page(1), "bucket").unwrap();
    let keys: Vec<&str> = listed.iter().map(|object| object.key.as_str()).collect();
    let mut expected = BACKEND_ORDER.to_vec();
    expected.sort();
    assert_eq!(keys, expected);
}

#[test]
fn a_cursor_on_a_key_that_prefixes_others_lists_each_key_once() {
    const R2_ORDER: [&str; 4] = [
        "generations/g/crates-io/si/gn/signal-hook-mio",
        "generations/g/crates-io/si/gn/signal-hook-registry",
        "generations/g/crates-io/si/gn/signal-hook",
        "generations/g/crates-io/si/gn/signature",
    ];
    let fake = FakeS3::start_verified();
    for key in R2_ORDER {
        fake.seed("bucket", key, key.as_bytes(), "text/plain");
    }
    let listed = list_all(&client(&fake).with_list_page(1), "bucket").unwrap();
    let keys: Vec<&str> = listed.iter().map(|object| object.key.as_str()).collect();
    let mut expected = R2_ORDER.to_vec();
    expected.sort();
    assert_eq!(keys, expected);
    assert_eq!(keys.len(), R2_ORDER.len());
}

#[test]
fn main_pulls_the_current_generation_through_s3() {
    let fake = FakeS3::start_verified();
    let objects: [(&str, &[u8]); 4] = [
        ("current", b"g"),
        ("generations/g/catalog.json", b"{\"gen\":1}"),
        ("generations/old/catalog.json", b"{\"gen\":0}"),
        ("blobs/0000.crate", b"crate bytes"),
    ];
    for (key, body) in objects {
        fake.seed("bucket", key, body, "text/plain");
    }
    let work = tempfile::tempdir().unwrap();
    let dest = work.path().join("dest");
    let report = pull(&client(&fake), "bucket", &dest, PullOptions::default()).unwrap();
    assert_eq!(report.generation, "g");
    assert_eq!(report.objects, 2);
    assert_eq!(report.verified, 0);
    assert_eq!(
        fs::read_link(dest.join("current")).unwrap(),
        PathBuf::from("generations/g")
    );
    assert_eq!(
        fs::read(dest.join("generations/g/catalog.json")).unwrap(),
        b"{\"gen\":1}"
    );
    assert!(!dest.join("generations/old").exists());
    assert_eq!(fs::read(dest.join("blobs/0000.crate")).unwrap(), b"crate bytes");
}

#[test]
fn pull_restores_the_generation_wheels_alias() {
    let fake = FakeS3::start_verified();
    let objects: [(&str, &[u8]); 3] = [
        ("current", b"g"),
        ("generations/g/catalog.json", b"{}"),
        ("wheels/pkg-1.0-py3-none-any.whl", b"wheel bytes"),
    ];
    for (key, body) in objects {
        fake.seed("bucket", key, body, "text/plain");
    }
    let work = tempfile::tempdir().unwrap();
    let dest = work.path().join("dest");
    pull(&client(&fake), "bucket", &dest, PullOptions::default()).unwrap();
    let alias = dest.join("generations/g/wheels");
    assert!(alias.symlink_metadata().is_ok());
    assert_eq!(fs::read_link(&alias).unwrap(), PathBuf::from("../../wheels"));
    assert_eq!(
        fs::read(dest.join("wheels/pkg-1.0-py3-none-any.whl")).unwrap(),
        b"wheel bytes"
    );
}

#[test]
fn a_fresh_cache_snapshot_fetches_only_the_shared_objects_a_publish_reads() {
    let config = Config::default();
    let cases = [
        (
            Registry::Cargo,
            config.cargo.bucket.clone(),
            "blobs/read.crate",
            "blobs/unread.crate",
        ),
        (
            Registry::Npm,
            config.npm.bucket.clone(),
            "tarballs/read.tgz",
            "tarballs/unread.tgz",
        ),
        (
            Registry::Pypi,
            config.pypi.bucket.clone(),
            "wheels/read.whl",
            "wheels/unread.whl",
        ),
        (
            Registry::Oci,
            config.oci.blobs_bucket.clone(),
            "sha256/read",
            "sha256/unread",
        ),
    ];
    for (registry, bucket, read_key, unread_key) in cases {
        let fake = FakeS3::start_verified();
        let metadata = metadata_bucket(registry, &config);
        fake.seed(&metadata, "generations/g/index", b"index", "text/plain");
        fake.seed(&bucket, read_key, b"read bytes", "application/octet-stream");
        fake.seed(&bucket, unread_key, b"unread bytes", "application/octet-stream");

        let work = tempfile::tempdir().unwrap();
        let snapshot = Snapshot::open(
            client(&fake),
            registry,
            &config,
            work.path().join("cache"),
        )
        .unwrap();
        assert!(
            object_get_paths(&fake).is_empty(),
            "{registry:?} fetched object bytes while opening the snapshot"
        );

        let store = shared_stores(registry, &config)
            .into_iter()
            .find(|store| store.bucket == bucket)
            .expect("shared store");
        assert!(store.contains(read_key));
        assert_eq!(
            snapshot.fetch_shared(&store, read_key).unwrap(),
            b"read bytes"
        );
        let expected = vec![format!("/{bucket}/{read_key}")];
        assert_eq!(object_get_paths(&fake), expected);
        assert_eq!(
            snapshot.fetch_shared(&store, read_key).unwrap(),
            b"read bytes"
        );
        assert_eq!(object_get_paths(&fake), expected);
    }
}
