#![forbid(unsafe_code)]

mod support;

use std::io::Write as _;

use stuff::publish::r2::multipart;
use stuff::publish::r2::{Credentials, RawRequest, R2Client};
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

fn artifact(payload: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("artifact");
    file.write_all(payload).expect("artifact body");
    file
}

fn state(fake: &FakeS3, key: &str) -> support::ObjectState {
    fake.state()
        .into_iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| panic!("missing object {key}"))
}

#[test]
fn aws_get_object_vector_is_accepted_by_the_verifier() {
    let fake = FakeS3::start_verified();
    fake.seed("bucket", "test.txt", b"hello world", "text/plain");
    let client = client(&fake);

    let response = client
        .request(RawRequest {
            method: "GET",
            bucket: "bucket",
            key: "test.txt",
            query: Vec::new(),
            body: &[],
            content_type: None,
            extra_headers: vec![("range", "bytes=0-9")],
        })
        .expect("signed GET");

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello world");
    assert_eq!(fake.requests()[0].method, "GET");
}

#[test]
fn initiation_canonicalizes_empty_query_as_uploads_equals() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);

    let upload_id = multipart::initiate(&client, "bucket", "path/artifact").expect("initiate");

    assert_eq!(upload_id, "test-upload-1");
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/bucket/path/artifact");
    assert!(requests[0].body.is_empty());
}

#[test]
fn part_upload_sends_the_part_content_type() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);
    let upload_id = multipart::initiate(&client, "bucket", "path/artifact").unwrap();

    let etag = multipart::upload_part(&client, "bucket", "path/artifact", 1, &upload_id, b"hello")
        .expect("signed part PUT");

    assert_eq!(etag.trim_matches('"'), multipart::md5_hex(b"hello"));
    let part = fake
        .requests()
        .into_iter()
        .find(|request| request.method == "PUT")
        .expect("part request");
    assert_eq!(part.path, "/bucket/path/artifact");
    assert_eq!(part.body, b"hello");
}

#[test]
fn completion_canonicalizes_the_upload_id_query() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);
    let upload_id = multipart::initiate(&client, "bucket", "path/artifact").unwrap();
    let first = multipart::upload_part(&client, "bucket", "path/artifact", 1, &upload_id, b"abcd")
        .unwrap();
    let second = multipart::upload_part(&client, "bucket", "path/artifact", 2, &upload_id, b"ij")
        .unwrap();

    multipart::complete(
        &client,
        "bucket",
        "path/artifact",
        &upload_id,
        &[(1, first), (2, second)],
        "text/plain",
    )
    .expect("signed completion");

    let stored = state(&fake, "path/artifact");
    assert_eq!(stored.body, b"abcdij");
    assert_eq!(
        stored.etag.trim_matches('"'),
        format!("{}-2", {
            let mut joined = Vec::new();
            joined.extend_from_slice(&digest_bytes(b"abcd"));
            joined.extend_from_slice(&digest_bytes(b"ij"));
            multipart::md5_hex(&joined)
        })
    );
}

#[test]
fn session_token_is_signed_and_sent() {
    let fake = FakeS3::start_verified();
    let mut credentials = credentials(&fake);
    credentials.session_token = Some("temporary-token".to_string());
    let client = R2Client::new(credentials);

    let upload_id = multipart::initiate(&client, "bucket", "path/artifact").expect("signed POST");

    assert_eq!(upload_id, "test-upload-1");
}

#[test]
fn detects_error_body_inside_http_200() {
    let body =
        b"<?xml version=\"1.0\"?><Error><Code>InternalError</Code><Message>try again</Message></Error>";
    assert_eq!(multipart::completion_error(body), "InternalError");
    assert_eq!(multipart::completion_error(b""), "");
}

#[test]
fn uploads_parts_and_completes() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);
    let payload = b"abcdefghij";
    let file = artifact(payload);
    let expected = multipart::multipart_etag_file(file.path(), 4).unwrap();

    let upload = multipart::upload_file(
        &client,
        "bucket",
        "path/artifact",
        file.path(),
        "text/plain",
        4,
    )
    .expect("multipart upload");

    assert_eq!(upload.size, payload.len() as u64);
    assert_eq!(upload.parts, 3);
    assert_eq!(upload.etag, expected);
    let requests = fake.requests();
    assert_eq!(requests[0].method, "POST");
    assert!(requests[0].body.is_empty());
    let parts: Vec<&[u8]> = requests
        .iter()
        .filter(|request| request.method == "PUT")
        .map(|request| request.body.as_slice())
        .collect();
    assert_eq!(parts, vec![b"abcd".as_slice(), b"efgh", b"ij"]);
    let completion = requests
        .iter()
        .rfind(|request| request.method == "POST" && !request.body.is_empty())
        .expect("completion request");
    assert_eq!(completion.method, "POST");
    assert!(
        completion
            .body
            .windows(b"<PartNumber>3</PartNumber>".len())
            .any(|window| window == b"<PartNumber>3</PartNumber>"),
        "{}",
        String::from_utf8_lossy(&completion.body)
    );
    let stored = state(&fake, "path/artifact");
    assert_eq!(stored.body, payload);
    assert_eq!(stored.etag.trim_matches('"'), expected);
    assert_eq!(stored.content_type, "text/plain");
}

#[test]
fn a_failed_part_aborts_the_multipart_upload() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);
    let file = artifact(b"abcdefghij");
    let upload_id = multipart::initiate(&client, "bucket", "path/artifact").unwrap();
    fake.inject_failures(1, 400, "0");

    let error = multipart::upload_parts(
        &client,
        "bucket",
        "path/artifact",
        file.path(),
        &upload_id,
        "text/plain",
        4,
    )
    .unwrap_err();

    assert_eq!(error.exit_code(), 5, "{error}");
    assert!(error.to_string().contains("HTTP 400"), "{error}");
    let requests = fake.requests();
    assert_eq!(requests.last().expect("abort request").method, "DELETE");
    assert!(!fake.state().iter().any(|entry| entry.key == "path/artifact"));
}

fn digest_bytes(chunk: &[u8]) -> Vec<u8> {
    let hex = multipart::md5_hex(chunk);
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("digest hex"))
        .collect()
}
