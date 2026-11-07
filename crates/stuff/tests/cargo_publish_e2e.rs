mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use deser_value::Value;
use stuff::publish::cargo::wirejson::json_string;
use stuff::publish::{
    CargoPublish, Options, PublishRequest, Registry, Request, Status,
};
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const TOKEN: &str = "stuff-cargo-publish-token";
const PUBLIC_BASE_URL: &str = "https://crates.example.test";
const REGISTRY: &str = "stuff-e2e";
const PACKAGE: &str = "stuff-cargo-publish-fixture";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    manifest_dir().join("../..")
}

fn fixture_crate() -> PathBuf {
    manifest_dir().join("tests/fixtures/cargo-publish")
}

fn scratch_root() -> tempfile::TempDir {
    let base = repo_root().join(".work/tmp");
    fs::create_dir_all(&base).expect("create .work/tmp");
    tempfile::Builder::new()
        .prefix("stuff-cargo-publish-e2e.")
        .tempdir_in(&base)
        .expect("scratch directory under .work/tmp")
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create copy target");
    for entry in fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("source entry");
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("source type").is_dir() {
            copy_tree(&path, &destination);
        } else {
            fs::copy(&path, &destination).expect("copy fixture file");
        }
    }
}

fn write_config(work: &Path) {
    let config_root = work.join("config");
    fs::create_dir_all(&config_root).expect("create config root");
    fs::write(
        config_root.join("config.json"),
        format!(
            "{{\"cargo\":{{\"bucket\":\"stuff-mycrates\",\"public_base_url\":{}}}}}\n",
            json_string(PUBLIC_BASE_URL)
        ),
    )
    .expect("write stuff config");
}

fn set_env(work: &Path, fake: &FakeS3) {
    let pairs = [
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("R2_S3_REGION", "auto".to_string()),
        ("CARGO_HTTP_PROXY", "http://127.0.0.1:1".to_string()),
        ("NO_PROXY", "127.0.0.1,localhost".to_string()),
    ];
    // SAFETY: this test binary has a single test, which sets these variables
    // before it starts any thread, child process, or library call that reads them.
    unsafe {
        for (name, value) in pairs {
            std::env::set_var(name, value);
        }
    }
}

fn publish_request(manifest: &Path) -> Request {
    Request::Publish {
        request: PublishRequest::Cargo(CargoPublish {
            packages: vec![PACKAGE.to_string()],
            manifest_path: Some(manifest.display().to_string()),
            registry: Some(REGISTRY.to_string()),
        }),
        options: Options::default(),
    }
}

struct PythonServer {
    child: Child,
    port: u16,
}

impl PythonServer {
    fn start(work: &Path) -> Self {
        let state = work.join("python-state");
        let site = work.join("python-site");
        let config = state.join("config.json");
        fs::create_dir_all(&state).expect("create python state");
        fs::copy(
            repo_root().join("crates/mycrates/tests/golden/adopt-publish/config.json"),
            &config,
        )
        .expect("copy the python config");
        let script = repo_root().join("crates/mycrates/bin/mycrates");
        let mut command = Command::new("python3");
        command
            .arg(&script)
            .arg("serve-publish")
            .args(["--bind", "127.0.0.1:0"])
            .arg("--state-dir")
            .arg(&state)
            .arg("--site-dir")
            .arg(&site)
            .arg("--config")
            .arg(&config)
            .env("MYCRATES_TOKEN", TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().expect("start the python oracle server");
        let stdout = child.stdout.take().expect("python stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read the python server address");
        let port = line
            .trim()
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .unwrap_or_else(|| panic!("unexpected python server output: {line:?}"));
        Self { child, port }
    }

    fn publish(&self, payload: &[u8]) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect to the python server");
        let head = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.port,
            payload.len()
        );
        stream.write_all(head.as_bytes()).expect("write head");
        stream.write_all(payload).expect("write payload");
        stream.flush().expect("flush payload");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.starts_with("HTTP/1.1 200 "),
            "python publish failed: {text}"
        );
    }
}

impl Drop for PythonServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn python_publish(work: &Path) -> String {
    let output = Command::new("python3")
        .arg(repo_root().join("crates/mycrates/bin/mycrates"))
        .arg("publish")
        .arg("--state-dir")
        .arg(work.join("python-state"))
        .arg("--site-dir")
        .arg(work.join("python-site"))
        .arg("--config")
        .arg(work.join("python-state/config.json"))
        .output()
        .expect("run the python publisher");
    assert!(
        output.status.success(),
        "python publish failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn single_native_record(state_dir: &Path) -> (String, String, String) {
    let native = state_dir.join("native");
    let crate_dir = fs::read_dir(&native)
        .expect("read native records")
        .map(|entry| entry.expect("native entry").path())
        .find(|path| path.is_dir())
        .expect("a native record directory");
    let record = fs::read_dir(&crate_dir)
        .expect("read crate records")
        .map(|entry| entry.expect("record entry").path())
        .find(|path| path.extension().is_some_and(|extension| extension == "json"))
        .expect("a native record");
    let value: Value = deser_json::from_str(&fs::read_to_string(&record).expect("read record"))
        .expect("native record json");
    let map = value.as_map().expect("native record object");
    let text = |name: &str| {
        map.get(name)
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| panic!("native record has no {name}"))
            .to_string()
    };
    let entry = map
        .get("entry")
        .and_then(|value| value.as_map())
        .expect("native record entry");
    assert!(
        entry
            .get("deps")
            .and_then(|value| value.kind().as_seq())
            .is_some_and(|deps| deps.is_empty()),
        "the fixture must have no dependencies"
    );
    assert!(
        entry
            .get("features")
            .and_then(|value| value.kind().as_map())
            .is_some_and(|features| features.is_empty()),
        "the fixture must have no features"
    );
    (text("name"), text("vers"), text("checksum"))
}

fn publish_payload(state_dir: &Path, name: &str, vers: &str, checksum: &str) -> Vec<u8> {
    let archive = fs::read(state_dir.join("blobs").join(format!("{checksum}.crate")))
        .expect("read the ingested crate archive");
    let metadata = format!(
        "{{\"name\":{},\"vers\":{},\"deps\":[],\"features\":{{}},\"links\":null,\"rust_version\":null,\"checksum\":{},\"yanked\":false}}",
        json_string(name),
        json_string(vers),
        json_string(checksum)
    );
    let mut payload = Vec::new();
    payload.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    payload.extend_from_slice(metadata.as_bytes());
    payload.extend_from_slice(&(archive.len() as u32).to_le_bytes());
    payload.extend_from_slice(&archive);
    payload
}

fn generation_id(site: &Path) -> String {
    let target = fs::read_link(site.join("current")).expect("current symlink");
    target
        .to_str()
        .and_then(|text| text.strip_prefix("generations/"))
        .expect("current points into generations")
        .to_string()
}

fn generation_files(site: &Path, generation: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_files(
        &site.join("generations").join(generation),
        &site.join("generations").join(generation),
        &mut files,
    );
    files
}

fn collect_files(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(directory).expect("read generation directory") {
        let entry = entry.expect("generation entry");
        let path = entry.path();
        if entry.file_type().expect("generation type").is_dir() {
            collect_files(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root).expect("relative path").to_path_buf(),
                fs::read(&path).expect("read generation file"),
            );
        }
    }
}

fn put_paths(fake: &FakeS3, after: usize) -> Vec<String> {
    fake.requests()
        .into_iter()
        .skip(after)
        .filter(|request| request.method == "PUT")
        .map(|request| request.path)
        .collect()
}

#[test]
fn cargo_publish_round_trips_and_republishes_unchanged() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    set_env(work.path(), &fake);
    write_config(work.path());

    let crate_dir = work.path().join("crate");
    copy_tree(&fixture_crate(), &crate_dir);
    let manifest = crate_dir.join("Cargo.toml");

    let first = stuff::publish::run(publish_request(&manifest)).expect("first cargo publish");
    assert_eq!(
        first,
        stuff::publish::Outcome {
            registry: Registry::Cargo,
            status: Status::Published,
        }
    );

    let state_dir = work.path().join("state/cargo");
    let rust_site = work.path().join("cache/cargo/site");
    let rust_generation = generation_id(&rust_site);
    assert_eq!(rust_generation.len(), 24);
    let remote: Vec<_> = fake
        .state()
        .into_iter()
        .filter(|object| object.bucket == "stuff-mycrates")
        .collect();
    assert!(
        remote
            .iter()
            .any(|object| object.key == format!("manifests/{rust_generation}.json")),
        "the generation manifest was not uploaded"
    );
    let current = remote
        .iter()
        .find(|object| object.key == "current")
        .expect("the current marker was uploaded");
    assert_eq!(current.body, rust_generation.as_bytes());

    let (name, vers, checksum) = single_native_record(&state_dir);
    assert_eq!((name.as_str(), vers.as_str()), (PACKAGE, "0.1.0"));
    let python = PythonServer::start(work.path());
    python.publish(&publish_payload(&state_dir, &name, &vers, &checksum));
    let reported = python_publish(work.path());
    assert!(reported.contains("noop: true"), "{reported}");
    let python_site = work.path().join("python-site");
    let python_generation = generation_id(&python_site);
    assert_eq!(
        python_generation, rust_generation,
        "generation ids differ: {reported}"
    );
    let python_files = generation_files(&python_site, &python_generation);
    let rust_files = generation_files(&rust_site, &rust_generation);
    assert_eq!(
        rust_files.keys().collect::<Vec<_>>(),
        python_files.keys().collect::<Vec<_>>()
    );
    for (relative, expected) in &python_files {
        assert_eq!(&rust_files[relative], expected, "{}", relative.display());
    }
    println!(
        "generation {rust_generation} matches the python oracle ({} files)",
        rust_files.len()
    );
    drop(python);

    let requests_before = fake.requests().len();
    let conditions_before = fake.put_conditions().len();
    let second = stuff::publish::run(publish_request(&manifest)).expect("second cargo publish");
    assert_eq!(
        second,
        stuff::publish::Outcome {
            registry: Registry::Cargo,
            status: Status::Unchanged,
        }
    );
    let puts = put_paths(&fake, requests_before);
    assert!(puts.is_empty(), "the no-op wrote remote objects: {puts:?}");
    assert_eq!(fake.put_conditions().len(), conditions_before);
    assert_eq!(generation_id(&rust_site), rust_generation);
    let blobs = fs::read_dir(state_dir.join("blobs"))
        .expect("read cargo blobs")
        .count();
    assert_eq!(blobs, 1, "a republish must not create another archive");
    println!(
        "second run returned unchanged with no PUT and {} promote conditions",
        conditions_before
    );
}
