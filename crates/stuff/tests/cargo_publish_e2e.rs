#![forbid(unsafe_code)]

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
    CargoPublish, Environment, Options, PublishRequest, Registry, Request, Status,
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

fn environment(work: &Path, fake: &FakeS3) -> Environment {
    let mut pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| !name.starts_with("STUFF_") && !name.starts_with("R2_"))
        .collect();
    for (name, value) in [
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("R2_S3_REGION", "auto".to_string()),
        ("CARGO_HTTP_PROXY", "http://127.0.0.1:1".to_string()),
        ("NO_PROXY", "127.0.0.1,localhost".to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
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

fn dry_run_request(manifest: &Path) -> Request {
    match publish_request(manifest) {
        Request::Publish { request, mut options } => {
            options.dry_run = true;
            Request::Publish { request, options }
        }
        other => other,
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

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if root.exists() {
        collect_tree(root, root, &mut files);
    }
    files
}

fn collect_tree(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .expect("read tree directory")
        .map(|entry| entry.expect("tree entry"))
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().expect("tree file type");
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_tree(root, &path, files);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        files.insert(
            path.strip_prefix(root).expect("relative tree path").to_path_buf(),
            fs::read(&path).expect("read tree file"),
        );
    }
}

fn remote_snapshot(fake: &FakeS3) -> Vec<(String, String, u64, String, Vec<u8>)> {
    let mut objects: Vec<_> = fake
        .state()
        .into_iter()
        .map(|object| (object.bucket, object.key, object.size, object.etag, object.body))
        .collect();
    objects.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    objects
}

fn mutation_count(fake: &FakeS3, after: usize) -> usize {
    fake.requests()[after..]
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count()
}

#[test]
fn cargo_publish_round_trips_and_republishes_unchanged() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let environment = environment(work.path(), &fake);
    write_config(work.path());

    let crate_dir = work.path().join("crate");
    copy_tree(&fixture_crate(), &crate_dir);
    let manifest = crate_dir.join("Cargo.toml");

    let first =
        stuff::publish::run(publish_request(&manifest), &environment).expect("first cargo publish");
    assert_eq!(first.registry, Registry::Cargo);
    assert_eq!(first.status, Status::Published);

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
    let second =
        stuff::publish::run(publish_request(&manifest), &environment).expect("second cargo publish");
    assert_eq!(second.registry, Registry::Cargo);
    assert_eq!(second.status, Status::Unchanged);
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

#[test]
fn a_cargo_dry_run_leaves_the_local_store_and_remote_unchanged() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let environment = environment(work.path(), &fake);
    write_config(work.path());

    let crate_dir = work.path().join("crate");
    copy_tree(&fixture_crate(), &crate_dir);
    let manifest = crate_dir.join("Cargo.toml");

    let first =
        stuff::publish::run(publish_request(&manifest), &environment).expect("first cargo publish");
    assert_eq!(first.status, Status::Published, "{first:?}");

    let state_dir = work.path().join("state/cargo");
    let rust_site = work.path().join("cache/cargo/site");
    let state_before = tree(&state_dir);
    let site_before = tree(&rust_site);
    let generation_before = generation_id(&rust_site);
    let remote_before = remote_snapshot(&fake);
    let requests_before = fake.requests().len();
    let conditions_before = fake.put_conditions().len();
    assert!(!state_before.is_empty(), "the first publish wrote no local state");
    assert!(!site_before.is_empty(), "the first publish wrote no local site");

    let dry = stuff::publish::run(dry_run_request(&manifest), &environment).expect("dry run");
    assert_eq!(dry.registry, Registry::Cargo);

    assert_eq!(tree(&state_dir), state_before, "the dry run changed local state");
    assert_eq!(tree(&rust_site), site_before, "the dry run changed the local site");
    assert_eq!(generation_id(&rust_site), generation_before);
    assert_eq!(remote_snapshot(&fake), remote_before, "the dry run changed the remote");
    assert_eq!(fake.put_conditions().len(), conditions_before);
    assert_eq!(mutation_count(&fake, requests_before), 0, "the dry run mutated the remote");
}

#[test]
fn a_native_crate_depending_on_a_published_native_crate_verifies_and_publishes() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let environment = environment(work.path(), &fake);
    write_config(work.path());

    let base_dir = work.path().join("base");
    copy_tree(&fixture_crate(), &base_dir);
    let base = stuff::publish::run(publish_request(&base_dir.join("Cargo.toml")), &environment)
        .expect("publish the dependency");
    assert_eq!(base.status, Status::Published, "{base:?}");

    let dependent_dir = work.path().join("dependent");
    fs::create_dir_all(dependent_dir.join("src")).expect("create dependent source");
    fs::write(
        dependent_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"stuff-cargo-publish-dependent\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"Depends on a native crate that is already published\"\nlicense = \"Apache-2.0\"\n\n[dependencies]\n{PACKAGE} = {{ version = \"0.1.0\", registry = \"{REGISTRY}\" }}\n\n[workspace]\n"
        ),
    )
    .expect("write dependent manifest");
    fs::write(dependent_dir.join("src/lib.rs"), "pub fn present() {}\n")
        .expect("write dependent source");

    let request = Request::Publish {
        request: PublishRequest::Cargo(CargoPublish {
            packages: vec!["stuff-cargo-publish-dependent".to_string()],
            manifest_path: Some(dependent_dir.join("Cargo.toml").display().to_string()),
            registry: Some(REGISTRY.to_string()),
        }),
        options: Options::default(),
    };
    let outcome = stuff::publish::run(request, &environment).expect("publish the dependent");
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
}

fn write_interdependent_workspace(root: &Path) -> PathBuf {
    for (name, dependency) in [
        ("stuff-interdependent-base", ""),
        (
            "stuff-interdependent-top",
            "stuff-interdependent-base = { version = \"0.1.0\", path = \"../stuff-interdependent-base\" }\n",
        ),
    ] {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("src")).expect("create crate source");
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"First-party crate released together with its dependents\"\nlicense = \"Apache-2.0\"\n\n[dependencies]\n{dependency}"
            ),
        )
        .expect("write crate manifest");
        fs::write(dir.join("src/lib.rs"), "pub fn present() {}\n").expect("write crate source");
    }
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"stuff-interdependent-base\", \"stuff-interdependent-top\"]\nresolver = \"2\"\n",
    )
    .expect("write workspace manifest");
    root.join("Cargo.toml")
}

fn replace_crates_io_with_an_empty_mirror(work: &Path) {
    let cargo_home = work.join("cache/cargo/cargo-home");
    let mirror = work.join("empty-mirror");
    fs::create_dir_all(&mirror).expect("create empty mirror");
    fs::create_dir_all(&cargo_home).expect("create cargo home");
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"empty-mirror\"\n\n[source.empty-mirror]\ndirectory = {}\n",
            json_string(&mirror.display().to_string())
        ),
    )
    .expect("write cargo config");
}

fn interdependent_request(manifest: &Path) -> Request {
    Request::Publish {
        request: PublishRequest::Cargo(CargoPublish {
            packages: vec![
                "stuff-interdependent-base".to_string(),
                "stuff-interdependent-top".to_string(),
            ],
            manifest_path: Some(manifest.display().to_string()),
            registry: Some(REGISTRY.to_string()),
        }),
        options: Options::default(),
    }
}

#[test]
fn crates_released_together_verify_against_each_other_instead_of_the_mirror() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let environment = environment(work.path(), &fake);
    write_config(work.path());
    replace_crates_io_with_an_empty_mirror(work.path());
    let workspace = work.path().join("workspace");
    let manifest = write_interdependent_workspace(&workspace);
    let base_manifest = workspace.join("stuff-interdependent-base/Cargo.toml");
    let released = fs::read_to_string(&base_manifest).expect("read base manifest");
    fs::write(&base_manifest, released.replace("0.1.0", "0.0.5")).expect("downgrade base");
    let earlier = stuff::publish::run(
        Request::Publish {
            request: PublishRequest::Cargo(CargoPublish {
                packages: vec!["stuff-interdependent-base".to_string()],
                manifest_path: Some(manifest.display().to_string()),
                registry: Some(REGISTRY.to_string()),
            }),
            options: Options::default(),
        },
        &environment,
    )
    .expect("publish an earlier version of the dependency");
    assert_eq!(earlier.status, Status::Published, "{earlier:?}");
    fs::write(&base_manifest, released).expect("restore base");

    let generated = Command::new("cargo")
        .args(["generate-lockfile", "--manifest-path"])
        .arg(&manifest)
        .output()
        .expect("run cargo generate-lockfile");
    assert!(generated.status.success(), "{generated:?}");
    let lockfile = manifest.with_file_name("Cargo.lock");
    let lock_before = fs::read(&lockfile).expect("read generated lockfile");

    let outcome = stuff::publish::run(interdependent_request(&manifest), &environment)
        .expect("publish interdependent crates");
    assert_eq!(
        fs::read(&lockfile).expect("read lockfile after publish"),
        lock_before,
        "publishing must leave the workspace lockfile as it was"
    );
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert_eq!(
        outcome.packages,
        vec![
            "stuff-interdependent-base@0.0.5".to_string(),
            "stuff-interdependent-base@0.1.0".to_string(),
            "stuff-interdependent-top@0.1.0".to_string(),
        ]
    );
}

#[test]
fn a_rerun_completes_the_crates_a_failed_run_left_unpromoted() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let environment = environment(work.path(), &fake);
    write_config(work.path());
    replace_crates_io_with_an_empty_mirror(work.path());
    let workspace = work.path().join("workspace");
    let manifest = write_interdependent_workspace(&workspace);
    let top_source = workspace.join("stuff-interdependent-top/src/lib.rs");
    fs::write(&top_source, "pub fn present() { missing(); }\n").expect("break the dependent");

    let failed = stuff::publish::run(interdependent_request(&manifest), &environment);
    assert!(failed.is_err(), "the broken dependent must fail verification");

    fs::write(&top_source, "pub fn present() {}\n").expect("repair the dependent");
    fs::write(
        workspace.join("stuff-interdependent-base/src/lib.rs"),
        "pub fn present() {}\npub fn changed_since_the_failed_run() {}\n",
    )
    .expect("change the dependency");

    let outcome = stuff::publish::run(interdependent_request(&manifest), &environment)
        .expect("rerun after the partial publish");
    assert_eq!(outcome.status, Status::Published, "{outcome:?}");
    assert_eq!(
        outcome.packages,
        vec![
            "stuff-interdependent-base@0.1.0".to_string(),
            "stuff-interdependent-top@0.1.0".to_string(),
        ]
    );
}
