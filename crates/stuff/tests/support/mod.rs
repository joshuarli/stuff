#![forbid(unsafe_code)]
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use deser_value::{Kind, Map, Value};
use h12tiny_client_sync::{http, Client};

const TIMEOUT: Duration = Duration::from_secs(5);

pub const ACCESS_KEY: &str = "test-access";
pub const SECRET_KEY: &str = "test-secret";

pub fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

pub fn write_executable(path: &std::path::Path, contents: &str) {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn shell to write {}: {error}", path.display()));
    child
        .stdin
        .take()
        .expect("shell stdin")
        .write_all(contents.as_bytes())
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    let status = child
        .wait()
        .unwrap_or_else(|error| panic!("wait for {}: {error}", path.display()));
    assert!(status.success(), "could not create {}: {status}", path.display());
}

pub fn seed_pypi_generation(
    wheel: &std::path::Path,
    targets: Vec<mypypi::models::Target>,
    output: &std::path::Path,
    public_base_url: &str,
) -> (String, bool) {
    let identity = mypypi::models::WheelIdentity::parse(
        wheel.file_name().and_then(|name| name.to_str()).expect("wheel filename"),
    )
    .expect("fixture wheel filename");
    let sha256 = mypypi::acquire::sha256_file(wheel).expect("hash the fixture wheel");
    let artifact =
        mypypi::models::ValidatedArtifact::new(identity, wheel.to_path_buf(), sha256, targets)
            .expect("validated fixture artifact");
    let (_, activation) = mypypi::store::publish(
        &[artifact],
        output,
        &rustc_hash::FxHashMap::default(),
        public_base_url,
    )
    .expect("seed the pypi generation");
    (activation.generation_id, activation.is_noop)
}

pub struct FakeS3 {
    child: Child,
    stdin: Option<ChildStdin>,
    base: String,
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct ObjectState {
    pub bucket: String,
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub struct RequestRecord {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct PutCondition {
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
    pub body: String,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl FakeS3 {
    pub fn start() -> Self {
        Self::spawn(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/fake_s3.py"
        ), false)
    }

    pub fn start_verified() -> Self {
        Self::spawn(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/fake_s3.py"
        ), true)
    }

    pub fn start_retry() -> Self {
        Self::spawn(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/fake_s3_retry.py"
        ), true)
    }

    fn spawn(script: &str, verify_signature: bool) -> Self {
        let mut command = Command::new("python3");
        command
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if verify_signature {
            command
                .arg("--verify-signature")
                .env("R2_S3_ACCESS_KEY_ID", ACCESS_KEY)
                .env("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY);
        }
        let mut child = command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn python3 {script}: {error}"));
        let stdout = child.stdout.take().expect("fake s3 stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read fake s3 port");
        let port: u16 = line
            .strip_prefix("PORT ")
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("unexpected fake s3 output: {line:?}"));
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            base: format!("http://127.0.0.1:{port}"),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn get(&self, path: &str) -> Reply {
        self.send(http::Method::GET, path, &[], &[])
    }

    pub fn head(&self, path: &str) -> Reply {
        self.send(http::Method::HEAD, path, &[], &[])
    }

    pub fn put(&self, path: &str, body: &[u8]) -> Reply {
        self.send(http::Method::PUT, path, body, &[])
    }

    pub fn put_with_headers(&self, path: &str, body: &[u8], headers: &[(&str, &str)]) -> Reply {
        self.send(http::Method::PUT, path, body, headers)
    }

    pub fn seed(&self, bucket: &str, key: &str, body: &[u8], content_type: &str) {
        let document = object(vec![
            ("bucket", text_value(bucket)),
            ("key", text_value(key)),
            (
                "body_base64",
                text_value(&base64::engine::general_purpose::STANDARD.encode(body)),
            ),
            ("content_type", text_value(content_type)),
        ]);
        let reply = self.control("/_fake/seed", &document);
        assert_eq!(reply.status, 200, "seed {bucket}/{key}");
    }

    pub fn state(&self) -> Vec<ObjectState> {
        let reply = self.control_get("/_fake/state");
        assert_eq!(reply.status, 200);
        let value: Value = deser_json::from_slice(&reply.body).expect("state json");
        let objects = value
            .as_map()
            .and_then(|map| map.get("objects"))
            .and_then(|value| value.kind().as_seq())
            .expect("state objects");
        objects
            .iter()
            .map(|entry| {
                let map = entry.as_map().expect("object entry");
                ObjectState {
                    bucket: text_field(map, "bucket"),
                    key: text_field(map, "key"),
                    size: map
                        .get("size")
                        .and_then(|value| value.kind().as_u64())
                        .expect("size"),
                    etag: text_field(map, "etag"),
                    content_type: text_field(map, "content_type"),
                    body: base64::engine::general_purpose::STANDARD
                        .decode(text_field(map, "body_base64"))
                        .expect("body_base64"),
                }
            })
            .collect()
    }

    pub fn requests(&self) -> Vec<RequestRecord> {
        let reply = self.control_get("/_fake/requests");
        assert_eq!(reply.status, 200);
        let value: Value = deser_json::from_slice(&reply.body).expect("request log json");
        let requests = value
            .as_map()
            .and_then(|map| map.get("requests"))
            .and_then(|value| value.kind().as_seq())
            .expect("request log");
        requests
            .iter()
            .map(|entry| {
                let map = entry.as_map().expect("request entry");
                RequestRecord {
                    method: text_field(map, "method"),
                    path: text_field(map, "path"),
                    body: base64::engine::general_purpose::STANDARD
                        .decode(text_field(map, "body_base64"))
                        .expect("request body_base64"),
                }
            })
            .collect()
    }

    pub fn reject_current(&self, count: u64, generation: Option<&str>) {
        let mut pairs = vec![("count", Value::from(count))];
        if let Some(generation) = generation {
            pairs.push(("generation", text_value(generation)));
        }
        let reply = self.control("/_fake/reject-current", &object(pairs));
        assert_eq!(reply.status, 200);
    }

    pub fn inject_failures(&self, count: u64, status: u16, retry_after: &str) {
        let document = object(vec![
            ("count", Value::from(count)),
            ("status", Value::from(u64::from(status))),
            ("retry_after", text_value(retry_after)),
        ]);
        let reply = self.control("/_fake/fail", &document);
        assert_eq!(reply.status, 200);
    }

    pub fn inject_resets(&self, count: u64) {
        let document = object(vec![("count", Value::from(count))]);
        let reply = self.control("/_fake/reset", &document);
        assert_eq!(reply.status, 200);
    }

    pub fn put_conditions(&self) -> Vec<PutCondition> {
        let reply = self.control_get("/_fake/conditions");
        assert_eq!(reply.status, 200);
        let value: Value = deser_json::from_slice(&reply.body).expect("conditions json");
        let entries = value
            .as_map()
            .and_then(|map| map.get("conditions"))
            .and_then(|value| value.kind().as_seq())
            .expect("conditions");
        entries
            .iter()
            .map(|entry| {
                let map = entry.as_map().expect("condition entry");
                PutCondition {
                    if_match: optional_text_field(map, "if_match"),
                    if_none_match: optional_text_field(map, "if_none_match"),
                    body: text_field(map, "body"),
                }
            })
            .collect()
    }

    pub fn change_current_on_head(&self, enabled: bool) {
        let document = object(vec![("enabled", Value::from(u64::from(enabled)))]);
        let reply = self.control("/_fake/current-on-head", &document);
        assert_eq!(reply.status, 200);
    }

    pub fn list_keys(&self, bucket: &str, page: u32) -> Vec<String> {
        let mut keys = Vec::new();
        let mut token = String::new();
        loop {
            let mut path = format!("/{bucket}?list-type=2&max-keys={page}");
            if !token.is_empty() {
                path.push_str("&continuation-token=");
                path.push_str(&token);
            }
            let reply = self.get(&path);
            assert_eq!(reply.status, 200, "list {bucket}");
            let text = String::from_utf8(reply.body).expect("listing utf-8");
            keys.extend(xml_values(&text, "Key"));
            if xml_text(&text, "IsTruncated").as_deref() != Some("true") {
                return keys;
            }
            token = xml_text(&text, "NextContinuationToken").expect("continuation token");
        }
    }

    fn control_get(&self, path: &str) -> Reply {
        self.send(http::Method::GET, path, &[], &[])
    }

    fn control(&self, path: &str, document: &Value) -> Reply {
        let body = deser_json::to_string(document).expect("control json");
        self.send(http::Method::POST, path, body.as_bytes(), &[])
    }

    fn send(
        &self,
        method: http::Method,
        path: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> Reply {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("{}{}", self.base, path));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(body.to_vec()).expect("build request");
        let client = Client::builder().connect_timeout(TIMEOUT).build();
        let mut response = client
            .request_with_timeout(request, Some(TIMEOUT))
            .expect("request fake s3");
        let status = response.status().as_u16();
        let headers_out = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let mut response_body = Vec::new();
        {
            let reader = response.body_mut();
            let _ = reader.set_read_timeout(Some(TIMEOUT));
            reader
                .read_to_end(&mut response_body)
                .expect("read fake s3 response");
        }
        Reply {
            status,
            headers: headers_out,
            body: response_body,
        }
    }
}

impl Drop for FakeS3 {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (name, value) in pairs {
        map.insert(Value::from(name.to_string()), value);
    }
    Value::from(Kind::Map(map))
}

fn text_value(text: &str) -> Value {
    Value::from(text.to_string())
}

fn text_field(map: &Map, name: &str) -> String {
    map.get(name)
        .and_then(|value| value.kind().as_str())
        .unwrap_or("")
        .to_string()
}

fn optional_text_field(map: &Map, name: &str) -> Option<String> {
    map.get(name)
        .and_then(|value| value.kind().as_str())
        .map(str::to_string)
}

fn xml_values(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        values.push(xml_unescape(&after[..end]));
        rest = &after[end + close.len()..];
    }
    values
}

fn xml_text(text: &str, tag: &str) -> Option<String> {
    xml_values(text, tag).into_iter().next()
}

fn xml_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
