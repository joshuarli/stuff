use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const ROOT_DIGEST: &str = "sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388";
const ARM64_DIGEST: &str = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
const REFERRER_DIGEST: &str = "sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25";

type Tamper = (&'static str, fn(&Path));

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn myoci(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_myoci"))
        .args(args)
        .output()
        .unwrap();
    Output {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn ok(args: &[&str]) -> String {
    let output = myoci(args);
    assert_eq!(output.code, 0, "myoci {args:?}: {}", output.stderr);
    output.stdout
}

fn fixture_layout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oci/layout")
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

struct Daemon {
    child: Child,
    address: String,
}

impl Daemon {
    fn spawn(args: &[&str]) -> Daemon {
        let mut child = Command::new(env!("CARGO_BIN_EXE_myoci"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line
            .trim()
            .strip_prefix("listening on ")
            .unwrap_or_else(|| panic!("unexpected startup line {line:?}"))
            .to_string();
        Daemon { child, address }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .unwrap_or_else(|| panic!("no {name} header in {:?}", self.headers))
            .1
            .as_str()
    }

    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).unwrap()
    }
}

fn http(address: &str, method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Response {
    let mut stream = TcpStream::connect(address).unwrap();
    let mut request = format!("{method} {target} HTTP/1.1\r\nhost: {address}\r\nconnection: close\r\ncontent-length: {}\r\n", body.len());
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let mut lines = head.lines();
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_string(), value.trim().to_string())
        })
        .collect();
    Response { status, headers, body: raw[split + 4..].to_vec() }
}

fn get(address: &str, target: &str) -> Response {
    http(address, "GET", target, &[("accept", "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json")], b"")
}

#[test]
fn imported_state_publishes_serves_adopts_and_detects_tampering() {
    let work = tempfile::tempdir().unwrap();
    let state = work.path().join("state");
    let site = work.path().join("site");
    let (state_arg, site_arg) = (path(&state), path(&site));
    let layout = fixture_layout();

    assert_eq!(ok(&["init", "--state-dir", state_arg]), format!("initialized {state_arg}\n"));
    let imported = ok(&["import", "--state-dir", state_arg, "--layout", path(&layout), "--repository", "example/app"]);
    assert_eq!(imported, format!("imported {ROOT_DIGEST} as example/app:stable (4 manifests, 6 blobs, 1 referrers)\n"));
    let repository_json = std::fs::read(state.join("repositories/example/app/repository.json")).unwrap();
    ok(&["import", "--state-dir", state_arg, "--layout", path(&layout), "--repository", "example/app"]);
    assert_eq!(std::fs::read(state.join("repositories/example/app/repository.json")).unwrap(), repository_json);

    let inspected = ok(&["inspect", "--state-dir", state_arg]);
    assert!(inspected.contains("tags: stable\n"), "{inspected}");
    assert!(inspected.contains(&format!("referrers: {ARM64_DIGEST}\n")), "{inspected}");
    assert!(ok(&["inspect", "--state-dir", state_arg, "--json"]).contains("\"tags\": [\"stable\"]"));
    assert_eq!(
        ok(&["verify", "--state-dir", state_arg]),
        "verified 1 repositories, 4 manifests, 6 blobs, 1 referrers\n"
    );

    let pristine = work.path().join("pristine");
    copy_dir(&state, &pristine);

    let published = ok(&["publish", "--state-dir", state_arg, "--site-dir", site_arg, "--public-url", "https://oci.example.test"]);
    let generation = published
        .strip_prefix("Published generation ")
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_else(|| panic!("{published}"))
        .to_string();
    assert_eq!(
        ok(&["publish", "--state-dir", state_arg, "--site-dir", site_arg, "--public-url", "https://oci.example.test"]),
        format!("Already published generation {generation}\n")
    );
    assert_eq!(std::fs::read_link(site.join("current")).unwrap(), Path::new(&format!("generations/{generation}")));
    let generation_dir = site.join("generations").join(&generation);
    let catalog = std::fs::read_to_string(generation_dir.join("catalog.json")).unwrap();
    assert!(catalog.contains("\"schema_version\": 4") || catalog.contains("\"schema_version\":4"), "{catalog}");
    assert!(catalog.contains("example/app"));
    assert!(std::fs::read_to_string(generation_dir.join("llms.txt")).unwrap().contains("https://oci.example.test"));
    assert!(generation_dir.join(format!("repositories/example/app/referrers/sha256/{}.json", &ARM64_DIGEST[7..])).is_file());

    let server = Daemon::spawn(&["serve", "--site-dir", site_arg]);
    let address = server.address.as_str();
    assert_eq!(get(address, "/v2/").status, 200);
    let manifest = get(address, "/v2/example/app/manifests/stable");
    assert_eq!(manifest.status, 200);
    assert_eq!(manifest.header("docker-content-digest"), ROOT_DIGEST);
    let by_digest = get(address, &format!("/v2/example/app/manifests/{ROOT_DIGEST}"));
    assert_eq!(by_digest.body, manifest.body);
    let tags = get(address, "/v2/example/app/tags/list");
    assert_eq!(tags.status, 200);
    assert!(tags.text().contains("\"stable\""), "{}", tags.text());
    let referrers = get(address, &format!("/v2/example/app/referrers/{ARM64_DIGEST}"));
    assert_eq!(referrers.status, 200);
    assert!(referrers.text().contains(REFERRER_DIGEST), "{}", referrers.text());
    assert_eq!(get(address, "/v2/example/app/manifests/missing").status, 404);
    assert_eq!(get(address, "/v2/nobody/manifests/stable").status, 404);
    let blob = std::fs::read_dir(state.join("blobs/sha256")).unwrap().next().unwrap().unwrap();
    let digest = format!("sha256:{}", blob.file_name().to_str().unwrap());
    let served = get(address, &format!("/v2/example/app/blobs/{digest}"));
    assert_eq!(served.status, 200);
    assert_eq!(served.body, std::fs::read(blob.path()).unwrap());
    drop(server);

    ok(&["tag", "--state-dir", state_arg, "--repository", "example/app", "--tag", "stable", "--digest", ARM64_DIGEST]);
    let moved = ok(&["publish", "--state-dir", state_arg, "--site-dir", site_arg]);
    assert!(moved.starts_with("Published generation "), "{moved}");
    assert!(!moved.contains(&generation));
    assert_eq!(
        ok(&["untag", "--state-dir", state_arg, "--repository", "example/app", "--tag", "stable"]),
        "removed example/app:stable\n"
    );
    ok(&["publish", "--state-dir", state_arg, "--site-dir", site_arg]);
    let current = std::fs::read_to_string(site.join("current/catalog.json")).unwrap();
    assert!(!current.contains("\"stable\""), "{current}");
    let absent = myoci(&["untag", "--state-dir", state_arg, "--repository", "example/app", "--tag", "stable"]);
    assert_eq!(absent.code, 2, "{}", absent.stderr);

    let adopted = work.path().join("adopted");
    let adopted_arg = path(&adopted);
    ok(&["init", "--state-dir", adopted_arg]);
    let summary = ok(&["adopt", "--state-dir", adopted_arg, "--site-dir", site_arg]);
    assert!(summary.starts_with("adopted generation "), "{summary}");
    ok(&["verify", "--state-dir", adopted_arg]);

    let tampers: [Tamper; 6] = [
        ("blob", |state| std::fs::write(first_file(&state.join("blobs/sha256")), b"tampered").unwrap()),
        ("manifest", |state| {
            let file = first_file(&state.join("manifests/sha256"));
            let mut bytes = std::fs::read(&file).unwrap();
            bytes.push(b' ');
            std::fs::write(file, bytes).unwrap();
        }),
        ("manifest bytes", |state| {
            let file = first_file(&state.join("manifests/sha256"));
            let mut bytes = std::fs::read(&file).unwrap();
            let last = bytes.len() - 1;
            bytes[last] = if bytes[last] == b'\n' { b' ' } else { b'\n' };
            std::fs::write(file, bytes).unwrap();
        }),
        ("descriptor size", |state| {
            let file = state.join("repositories/example/app/repository.json");
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.contains("\"size\":671"));
            std::fs::write(file, text.replace("\"size\":671", "\"size\":672")).unwrap();
        }),
        ("tag", |state| {
            let file = state.join("repositories/example/app/repository.json");
            let text = std::fs::read_to_string(&file).unwrap();
            std::fs::write(file, text.replace(&ARM64_DIGEST[7..], &"1".repeat(64))).unwrap();
        }),
        ("referrer index", |state| {
            let file = state.join(format!("repositories/example/app/referrers/sha256/{}.json", &ARM64_DIGEST[7..]));
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.contains("\"size\":671"));
            std::fs::write(file, text.replace("\"size\":671", "\"size\":672")).unwrap();
        }),
    ];
    for (name, tamper) in tampers {
        let copy = work.path().join(format!("tampered-{}", name.replace(' ', "-")));
        copy_dir(&pristine, &copy);
        tamper(&copy);
        let verified = myoci(&["verify", "--state-dir", path(&copy)]);
        assert_eq!(verified.code, 3, "tampered {name}: {}{}", verified.stdout, verified.stderr);
    }
}

#[test]
fn failures_map_to_the_documented_exit_codes() {
    let work = tempfile::tempdir().unwrap();
    let state = work.path().join("state");
    let state_arg = path(&state);
    let layout = fixture_layout();

    assert_eq!(myoci(&[]).code, 0);
    for args in [
        vec!["init"],
        vec!["import", "--state-dir", state_arg, "--repository", "a/b"],
        vec!["import", "--state-dir", state_arg, "--layout", "l", "--from", "f", "--repository", "a/b"],
        vec!["tag", "--state-dir", state_arg, "--repository", "a/b", "--tag", "t"],
        vec!["inspect", "--state-dir", state_arg, "--layout", "l"],
        vec!["serve", "--site-dir", state_arg, "--push"],
        vec!["frobnicate"],
    ] {
        let output = myoci(&args);
        assert_eq!(output.code, 2, "{args:?}: {}", output.stderr);
        assert!(output.stderr.starts_with("myoci: "), "{}", output.stderr);
    }

    ok(&["import", "--state-dir", state_arg, "--layout", path(&layout), "--repository", "example/app", "--tag", "stable"]);
    let source = tempfile::tempdir().unwrap();
    copy_dir(&layout, source.path());
    let index = source.path().join("index.json");
    let text = std::fs::read_to_string(&index).unwrap();
    assert!(text.contains(&ROOT_DIGEST[7..]));
    std::fs::write(&index, text.replace(&ROOT_DIGEST[7..], &ARM64_DIGEST[7..])).unwrap();
    let mismatched = myoci(&["import", "--state-dir", state_arg, "--layout", path(source.path()), "--repository", "example/app", "--tag", "stable"]);
    assert_eq!(mismatched.code, 3, "{}", mismatched.stderr);

    std::fs::remove_dir_all(state.join("blobs/sha256")).unwrap();
    std::fs::create_dir_all(state.join("blobs/sha256")).unwrap();
    assert_eq!(myoci(&["verify", "--state-dir", state_arg]).code, 3);
}

fn first_file(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir).unwrap().next().unwrap().unwrap().path()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn sha256_of(bytes: &[u8]) -> String {
    stuff_oci::Digest::from_bytes(bytes).as_str().to_string()
}

#[test]
fn pushed_blob_and_manifest_are_served_back_and_verified() {
    let work = tempfile::tempdir().unwrap();
    let state = work.path().join("state");
    let site = work.path().join("site");
    let token = work.path().join("token");
    let secret = work.path().join("secret");
    std::fs::write(&token, "push-token").unwrap();
    std::fs::write(&secret, "push-secret").unwrap();
    let (state_arg, site_arg) = (path(&state), path(&site));
    ok(&["init", "--state-dir", state_arg]);
    ok(&["publish", "--state-dir", state_arg, "--site-dir", site_arg]);

    let writer = Daemon::spawn(&["writer", "--state-dir", state_arg, "--bind", "127.0.0.1:0", "--secret-file", path(&secret)]);
    let writer_url = format!("http://{}", writer.address);
    let front = Daemon::spawn(&[
        "serve", "--site-dir", site_arg, "--state-dir", state_arg, "--push", "--writer-url", &writer_url,
        "--push-token-file", path(&token), "--push-secret-file", path(&secret),
    ]);
    let front_address = front.address.as_str();
    let auth = "Basic c3R1ZmY6cHVzaC10b2tlbg==";

    let config = b"{}".as_slice();
    let layer = b"pushed layer bytes".as_slice();
    let (config_digest, layer_digest) = (sha256_of(config), sha256_of(layer));
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{config_digest}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{layer_digest}","size":{}}}]}}"#,
        config.len(),
        layer.len()
    )
    .into_bytes();
    let manifest_digest = sha256_of(&manifest);
    let manifest_type = ("content-type", "application/vnd.oci.image.manifest.v1+json");

    let anonymous = http(front_address, "POST", "/v2/example/pushed/blobs/uploads/", &[], b"");
    assert_eq!(anonymous.status, 401);
    assert!(anonymous.header("www-authenticate").starts_with("Basic"));
    for credentials in ["Basic c3R1ZmY6bm9wZQ==", "Basic bm9wZTpwdXNoLXRva2Vu"] {
        let wrong = http(front_address, "POST", "/v2/example/pushed/blobs/uploads/", &[("authorization", credentials)], b"");
        assert_eq!(wrong.status, 401, "{credentials}");
    }

    let early = http(front_address, "PUT", "/v2/example/pushed/manifests/latest", &[("authorization", auth), manifest_type], &manifest);
    assert_eq!(early.status, 400, "{}", early.text());

    for (bytes, digest) in [(config, &config_digest), (layer, &layer_digest)] {
        let started = http(front_address, "POST", "/v2/example/pushed/blobs/uploads/", &[("authorization", auth)], b"");
        assert_eq!(started.status, 202, "{}", started.text());
        let location = started.header("location").to_string();
        let upload = location.strip_prefix(&writer_url).unwrap_or_else(|| panic!("{location}"));
        let tampered = http(&writer.address, "PUT", &format!("{upload}&digest={}", sha256_of(b"other")), &[], bytes);
        assert_eq!(tampered.status, 400, "{}", tampered.text());
        let started = http(front_address, "POST", "/v2/example/pushed/blobs/uploads/", &[("authorization", auth)], b"");
        let location = started.header("location").to_string();
        let upload = location.strip_prefix(&writer_url).unwrap();
        let (head, tail) = bytes.split_at(bytes.len() / 2);
        assert_eq!(http(&writer.address, "PATCH", upload, &[], head).status, 202);
        let finished = http(&writer.address, "PUT", &format!("{upload}&digest={digest}"), &[], tail);
        assert_eq!(finished.status, 201, "{}", finished.text());
    }

    let pushed = http(front_address, "PUT", "/v2/example/pushed/manifests/latest", &[("authorization", auth), manifest_type], &manifest);
    assert_eq!(pushed.status, 201, "{}", pushed.text());
    assert_eq!(pushed.header("docker-content-digest"), manifest_digest);

    let pulled = get(front_address, "/v2/example/pushed/manifests/latest");
    assert_eq!(pulled.status, 200);
    assert_eq!(pulled.body, manifest);
    let blob = get(front_address, &format!("/v2/example/pushed/blobs/{layer_digest}"));
    assert_eq!(blob.status, 200);
    assert_eq!(blob.body, layer);

    drop(front);
    drop(writer);
    assert!(ok(&["inspect", "--state-dir", state_arg]).contains("tags: latest\n"));
    ok(&["verify", "--state-dir", state_arg]);
    let current = std::fs::read_to_string(site.join("current/catalog.json")).unwrap();
    assert!(current.contains("example/pushed"), "{current}");
}
