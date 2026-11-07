#![forbid(unsafe_code)]

mod support;

use support::FakeS3;

#[test]
fn seeded_object_round_trips_through_get_and_head() {
    let fake = FakeS3::start();
    fake.seed("bucket", "blobs/a", b"artifact", "application/octet-stream");

    let get = fake.get("/bucket/blobs/a");
    assert_eq!(get.status, 200);
    assert_eq!(get.body, b"artifact");
    let etag = get.header("etag").expect("GET ETag").to_string();
    assert_eq!(etag.len(), 34);
    assert!(etag.starts_with('"') && etag.ends_with('"'));

    let head = fake.head("/bucket/blobs/a");
    assert_eq!(head.status, 200);
    assert_eq!(head.header("etag"), Some(etag.as_str()));
    assert!(head.body.is_empty());
}

#[test]
fn paginated_listing_returns_every_key_across_pages() {
    let fake = FakeS3::start();
    let mut keys: Vec<String> = (1..=5).map(|index| format!("generations/g/{index}")).collect();
    for key in &keys {
        fake.seed("bucket", key, key.as_bytes(), "text/plain");
    }
    keys.sort();

    assert_eq!(fake.list_keys("bucket", 2), keys);
    let list_requests = fake
        .requests()
        .iter()
        .filter(|request| request.method == "GET" && request.path == "/bucket")
        .count();
    assert!(
        list_requests >= 3,
        "expected at least three list pages, saw {list_requests}"
    );
}

#[test]
fn stale_if_match_put_gets_412_without_overwriting() {
    let fake = FakeS3::start();
    fake.seed("bucket", "current", b"g0", "text/plain");
    let before = fake.get("/bucket/current");
    assert_eq!(before.body, b"g0");
    let etag = before.header("etag").expect("current ETag").to_string();

    let stale = fake.put_with_headers("/bucket/current", b"g1", &[("if-match", "\"stale\"")]);
    assert_eq!(stale.status, 412);

    let unchanged = fake.get("/bucket/current");
    assert_eq!(unchanged.body, b"g0");
    assert_eq!(unchanged.header("etag"), Some(etag.as_str()));

    let fresh = fake.put_with_headers("/bucket/current", b"g1", &[("if-match", etag.as_str())]);
    assert_eq!(fresh.status, 200);
    assert_eq!(fake.get("/bucket/current").body, b"g1");
}

#[test]
fn injected_429_carries_retry_after() {
    let fake = FakeS3::start();
    fake.inject_failures(1, 429, "7");

    let throttled = fake.put("/bucket/key", b"body");
    assert_eq!(throttled.status, 429);
    assert_eq!(throttled.header("retry-after"), Some("7"));

    let accepted = fake.put("/bucket/key", b"body");
    assert_eq!(accepted.status, 200);
    let state = fake.state();
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].key, "key");
    assert_eq!(state[0].body, b"body");
}
