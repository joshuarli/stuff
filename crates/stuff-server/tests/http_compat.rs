use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use stuff_store::{write_file_durable, Site};
use tempfile::TempDir;

struct TestServer {
    child: Child,
    base_url: String,
    _tmp: TempDir,
}

impl TestServer {
    fn boot(site_dir: &Path, tmp: TempDir) -> Self {
        let bin = env!("CARGO_BIN_EXE_stuff-server");
        let mut child = Command::new(bin)
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--route")
            .arg(format!("/={}", site_dir.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn stuff-server");
        let stdout = child.stdout.take().expect("server stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("server banner");
        let addr = line
            .trim()
            .strip_prefix("listening on ")
            .expect("banner format")
            .to_string();
        drop(reader);
        Self {
            child,
            base_url: format!("http://{addr}"),
            _tmp: tmp,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn request(server: &TestServer, raw: &str) -> Response {
    let addr = server.base_url.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim().to_string();
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').unwrap();
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    Response {
        status,
        headers,
        body,
    }
}

fn get(server: &TestServer, path: &str, extra: &[&str]) -> Response {
    let mut raw = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    for h in extra {
        raw.push_str(h);
        raw.push_str("\r\n");
    }
    raw.push_str("\r\n");
    request(server, &raw)
}

fn fixture_site() -> (TempDir, PathBuf, String) {
    let tmp = TempDir::new().unwrap();
    let site_dir = tmp.path().join("site");
    let site = Site::new(site_dir.clone());
    site.ensure_layout().unwrap();
    let staged = site.stage().unwrap();
    let sp = staged.path().to_path_buf();
    write_file_durable(&sp.join("registry/config.json"), br#"{"dl":"http://x/{crate}"}"#).unwrap();
    let blob = vec![7u8; 4096];
    let blob_path = site_dir.join("blobs/abcd1234.crate");
    write_file_durable(&blob_path, &blob).unwrap();
    fs::create_dir_all(sp.join("nested")).unwrap();
    write_file_durable(&sp.join("nested/pkg"), b"scoped").unwrap();
    let staged_path = staged.into_path();
    let act = site
        .activate(staged_path, "gen9", &|_: &Path| Ok(true))
        .unwrap();
    assert!(!act.is_noop);
    (tmp, site_dir, "gen9".to_string())
}

#[test]
fn static_read_semantics() {
    let (tmp, site_dir, gen_id) = fixture_site();
    let server = TestServer::boot(&site_dir, tmp);

    let r = get(&server, "/current/registry/config.json", &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["content-type"], "application/json");
    assert_eq!(r.headers["cache-control"], "public, max-age=60");
    assert_eq!(r.headers["accept-ranges"], "bytes");
    assert!(r.headers.contains_key("etag"));
    assert_eq!(r.body, br#"{"dl":"http://x/{crate}"}"#);

    let r = get(&server, &format!("/generations/{gen_id}/registry/config.json"), &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["cache-control"], "public, max-age=31536000, immutable");

    let r = get(&server, "/blobs/abcd1234.crate", &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["content-type"], "application/x-tar");
    assert_eq!(r.body.len(), 4096);

    let r = get(&server, "/current/nested%2fpkg", &[]);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"scoped");

    let etag = get(&server, "/current/registry/config.json", &[]).headers["etag"].clone();
    let r = get(
        &server,
        "/current/registry/config.json",
        &[&format!("If-None-Match: {etag}")],
    );
    assert_eq!(r.status, 304);
    assert!(r.body.is_empty());

    let r = get(
        &server,
        "/current/registry/config.json",
        &[r#"If-Match: "nope""#],
    );
    assert_eq!(r.status, 412);

    let r = get(&server, "/current/registry/config.json", &["Range: bytes=0-3"]);
    assert_eq!(r.status, 206);
    assert_eq!(r.headers["content-range"], "bytes 0-3/25");
    assert_eq!(r.body, b"{\"dl");

    let r = get(&server, "/current/registry/config.json", &["Range: bytes=999-"]);
    assert_eq!(r.status, 416);

    let r = get(&server, "/nope", &[]);
    assert_eq!(r.status, 404);
    let r = get(&server, "/current/../../etc", &[]);
    assert_eq!(r.status, 404);
    let r = get(&server, "/current/%2e%2e/x", &[]);
    assert_eq!(r.status, 404);

    let raw = "HEAD /current/registry/config.json HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n";
    let r = request(&server, raw);
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["content-length"], "25");
    assert!(r.body.is_empty());

    let raw = "POST /current/registry/config.json HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let r = request(&server, raw);
    assert_eq!(r.status, 405);
}
