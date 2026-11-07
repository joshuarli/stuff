#![forbid(unsafe_code)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use stuff::publish::r2::{Credentials, Precondition, R2Client};
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

fn recorder() -> (Arc<Mutex<Vec<Duration>>>, impl Fn(Duration) + 'static) {
    let sleeps = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&sleeps);
    (sleeps, move |duration| {
        recorded.lock().expect("sleeper lock").push(duration);
    })
}

#[test]
fn put_get_head_round_trip_preserves_etag_and_content_type() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);

    let etag = client
        .put_object(
            "bucket",
            "blobs/a",
            b"artifact",
            "application/octet-stream",
            Precondition::None,
        )
        .expect("signed PUT");
    assert_eq!(etag.len(), 32, "single-part ETag is the bare MD5: {etag}");
    assert_eq!(client.get_object("bucket", "blobs/a").unwrap(), b"artifact");

    let metadata = client.head_object("bucket", "blobs/a").unwrap();
    assert_eq!(metadata.size, 8);
    assert_eq!(metadata.etag, etag);
    assert_eq!(
        metadata.content_type.as_deref(),
        Some("application/octet-stream")
    );
}

#[test]
fn list_objects_v2_follows_continuation_tokens_across_pages() {
    let fake = FakeS3::start_verified();
    let mut keys: Vec<String> =
        (1..=5).map(|index| format!("generations/g/{index}")).collect();
    for key in &keys {
        fake.seed("bucket", key, key.as_bytes(), "text/plain");
    }
    keys.sort();

    let client = R2Client::new(credentials(&fake)).with_list_page(2);
    let objects = client.list_objects_v2("bucket").unwrap();
    let listed: Vec<String> = objects.iter().map(|object| object.key.clone()).collect();
    assert_eq!(listed, keys);

    let state = fake.state();
    for object in &objects {
        let stored = state
            .iter()
            .find(|entry| entry.key == object.key)
            .unwrap_or_else(|| panic!("missing state for {}", object.key));
        assert_eq!(object.size, stored.size);
        assert_eq!(object.etag, stored.etag.trim_matches('"'));
    }

    let pages = fake
        .requests()
        .iter()
        .filter(|request| request.method == "GET" && request.path == "/bucket")
        .count();
    assert!(pages >= 3, "expected three list pages, saw {pages}");
}

#[test]
fn a_stale_if_match_put_is_a_conflict() {
    let fake = FakeS3::start_verified();
    fake.seed("bucket", "current", b"g0", "text/plain");
    let client = client(&fake);

    let error = client
        .put_object(
            "bucket",
            "current",
            b"g1",
            "text/plain",
            Precondition::IfMatch("\"stale\"".to_string()),
        )
        .unwrap_err();
    assert_eq!(error.exit_code(), 3, "{error}");
    assert_eq!(fake.state()[0].body, b"g0");
}

#[test]
fn if_none_match_any_on_an_existing_key_is_a_conflict() {
    let fake = FakeS3::start_verified();
    fake.seed("bucket", "current", b"g0", "text/plain");
    let client = client(&fake);

    let error = client
        .put_object(
            "bucket",
            "current",
            b"g1",
            "text/plain",
            Precondition::IfNoneMatchAny,
        )
        .unwrap_err();
    assert_eq!(error.exit_code(), 3, "{error}");
    assert_eq!(fake.state()[0].body, b"g0");
}

#[test]
fn retries_a_429_honoring_retry_after() {
    let fake = FakeS3::start_verified();
    fake.inject_failures(1, 429, "3");
    let (sleeps, sleeper) = recorder();
    let client = R2Client::new(credentials(&fake)).with_sleeper(sleeper);

    client
        .put_object("bucket", "blobs/retry", b"body", "text/plain", Precondition::None)
        .expect("retried PUT");

    assert_eq!(*sleeps.lock().unwrap(), vec![Duration::from_secs(3)]);
    let state = fake.state();
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].key, "blobs/retry");
    assert_eq!(state[0].body, b"body");
}

#[test]
fn repeated_5xx_becomes_a_transport_error() {
    let fake = FakeS3::start_verified();
    fake.inject_failures(10, 500, "1");
    let (sleeps, sleeper) = recorder();
    let client = R2Client::new(credentials(&fake))
        .with_attempts(3)
        .with_sleeper(sleeper);

    let error = client
        .put_object("bucket", "blobs/fail", b"body", "text/plain", Precondition::None)
        .unwrap_err();
    assert_eq!(error.exit_code(), 5, "{error}");
    assert_eq!(sleeps.lock().unwrap().len(), 2);
    assert!(fake.state().is_empty());
}

#[test]
fn a_request_signed_with_the_wrong_secret_is_rejected_as_config() {
    let fake = FakeS3::start_verified();
    let mut wrong = credentials(&fake);
    wrong.secret_access_key = "wrong-secret".to_string();
    let client = R2Client::new(wrong);

    let error = client
        .get_object("bucket", "blobs/a")
        .unwrap_err();
    assert_eq!(error.exit_code(), 2, "{error}");
    assert!(error.to_string().contains("SignatureDoesNotMatch"), "{error}");
}

#[test]
fn retries_after_a_connection_reset() {
    let fake = FakeS3::start_verified();
    fake.inject_resets(1);
    let (sleeps, sleeper) = recorder();
    let client = R2Client::new(credentials(&fake)).with_sleeper(sleeper);

    client
        .put_object("bucket", "blobs/reset", b"body", "text/plain", Precondition::None)
        .expect("retried PUT");

    assert_eq!(sleeps.lock().unwrap().len(), 1);
    let state = fake.state();
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].key, "blobs/reset");
    assert_eq!(state[0].body, b"body");
}

#[test]
fn connection_reset_with_single_attempt_becomes_a_transport_error() {
    let fake = FakeS3::start_verified();
    fake.inject_resets(1);
    let client = R2Client::new(credentials(&fake)).with_attempts(1);

    let error = client
        .put_object("bucket", "blobs/reset-fail", b"body", "text/plain", Precondition::None)
        .unwrap_err();
    assert_eq!(error.exit_code(), 5, "{error}");
    assert!(fake.state().is_empty());
}
