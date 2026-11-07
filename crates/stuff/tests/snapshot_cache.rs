#![forbid(unsafe_code)]

mod support;

use std::fs;

use stuff::publish::config::Config;
use stuff::publish::r2::{Credentials, R2Client};
use stuff::publish::snapshot::Snapshot;
use stuff::publish::Registry;
use support::{ACCESS_KEY, FakeS3, SECRET_KEY};

const BUCKET: &str = "stuff-mycrates";
const KEY: &str = "generations/g1/catalog.json";

fn client(fake: &FakeS3) -> R2Client {
    R2Client::new(Credentials {
        endpoint: fake.base_url().to_string(),
        access_key_id: ACCESS_KEY.to_string(),
        secret_access_key: SECRET_KEY.to_string(),
        region: "auto".to_string(),
        session_token: None,
    })
}

fn gets(fake: &FakeS3, after: usize) -> Vec<String> {
    fake.requests()
        .into_iter()
        .skip(after)
        .filter(|request| {
            request.method == "GET" && request.path == format!("/{BUCKET}/{KEY}")
        })
        .map(|request| request.path)
        .collect()
}

#[test]
fn cached_objects_are_revalidated_and_refetched_when_the_listing_changes() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().expect("work directory");
    let cache = work.path().join("cache");
    let config = Config::default();
    fake.seed(BUCKET, KEY, b"first\n", "application/json");

    let snapshot = Snapshot::open(client(&fake), Registry::Cargo, &config, cache.clone())
        .expect("first snapshot");
    let before = fake.requests().len();
    assert_eq!(snapshot.fetch(BUCKET, KEY).unwrap(), b"first\n");
    assert_eq!(gets(&fake, before).len(), 1, "the first fetch was not network");

    let before = fake.requests().len();
    assert_eq!(snapshot.fetch(BUCKET, KEY).unwrap(), b"first\n");
    assert_eq!(
        gets(&fake, before),
        Vec::<String>::new(),
        "a valid cache hit refetched the object"
    );

    fake.seed(BUCKET, KEY, b"second\n", "application/json");
    let snapshot = Snapshot::open(client(&fake), Registry::Cargo, &config, cache.clone())
        .expect("second snapshot");
    let before = fake.requests().len();
    assert_eq!(
        snapshot.fetch(BUCKET, KEY).unwrap(),
        b"second\n",
        "a stale cache entry was served without revalidation"
    );
    assert_eq!(gets(&fake, before).len(), 1, "the stale object was not refetched");
}

#[test]
fn a_corrupt_cache_entry_is_refetched() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().expect("work directory");
    let cache = work.path().join("cache");
    let config = Config::default();
    fake.seed(BUCKET, KEY, b"payload\n", "application/json");
    let snapshot = Snapshot::open(client(&fake), Registry::Cargo, &config, cache.clone())
        .expect("snapshot");
    assert_eq!(snapshot.fetch(BUCKET, KEY).unwrap(), b"payload\n");

    fs::write(cache.join(BUCKET).join(KEY), b"corrupt").unwrap();
    let snapshot = Snapshot::open(client(&fake), Registry::Cargo, &config, cache.clone())
        .expect("snapshot after corruption");
    let before = fake.requests().len();
    assert_eq!(snapshot.fetch(BUCKET, KEY).unwrap(), b"payload\n");
    assert_eq!(gets(&fake, before).len(), 1, "the corrupt cache was trusted");
}
