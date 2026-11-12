#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use deser_value::Value;
use mypypi::models::{Target, ValidatedArtifact, WheelIdentity};
use stuff::publish::config::Config;
use stuff::publish::r2::{Credentials, R2Client};
use support::{write_executable, FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mypypi";
const FIRST_PARTY_WHEEL: &str = "first_party_demo-0.1.0-py3-none-any.whl";
const MIRRORED_WHEEL: &str = "validation_fixture-1.0-py3-none-any.whl";

const FAKE_PYTHON: &str = r#"#!/bin/sh
case "$*" in
*implementation.name*) echo 'cpython 3 15' ;;
*version_info.micro*) echo '3 15 1' ;;
*sys.executable*) echo /fake/python ;;
*cache_tag*) printf 'cpython\n3.15.1\ncpython-315\nfake-platform\n' ;;
esac
"#;

const WHEEL_SCRIPT: &str = r#"
import base64, hashlib, sys, zipfile
path, name, version, tag, body, tamper = sys.argv[1:7]
module = name.replace("-", "_")
py, abi, plat = tag.split("-")
tags = "".join(f"Tag: {p}-{a}-{l}\n" for p in py.split(".") for a in abi.split(".") for l in plat.split("."))
dist = f"{module}-{version}.dist-info"
files = {
    f"{module}/__init__.py": body.encode(),
    f"{dist}/METADATA": f"Metadata-Version: 2.4\nName: {name}\nVersion: {version}\nRequires-Python: >=3.15,<3.16\n\n".encode(),
    f"{dist}/WHEEL": f"Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\n{tags}".encode(),
}
def digest(data):
    return base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
record = ""
for member, data in files.items():
    shown = b"tampered" if tamper == "1" and member.endswith("__init__.py") else data
    record += f"{member},sha256={digest(shown)},{len(data)}\n"
record += f"{dist}/RECORD,,\n"
with zipfile.ZipFile(path, "w") as archive:
    for member, data in files.items():
        archive.writestr(member, data)
    archive.writestr(f"{dist}/RECORD", record)
"#;

fn build_wheel(dir: &Path, filename: &str, name: &str, version: &str, body: &str, tamper: bool) -> PathBuf {
    let path = dir.join(filename);
    let tag = filename
        .trim_end_matches(".whl")
        .splitn(3, '-')
        .nth(2)
        .expect("wheel tags")
        .to_string();
    let status = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(WHEEL_SCRIPT)
        .args([path.to_str().unwrap(), name, version, &tag, body, if tamper { "1" } else { "0" }])
        .status()
        .expect("run python3 to build a wheel");
    assert!(status.success(), "python3 could not build {filename}");
    path
}

struct Workspace {
    fake: FakeS3,
    root: tempfile::TempDir,
    bin: PathBuf,
    wheels: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        write_executable(&bin.join("python"), FAKE_PYTHON);
        write_executable(
            &bin.join("uv"),
            &format!(
                r#"#!/bin/sh
case "$1" in
--version) echo "uv {version} (fake)" ;;
venv)
  for last; do :; done
  mkdir -p "$last/bin"
  printf '#!/bin/sh\nexit 0\n' > "$last/bin/python"
  chmod +x "$last/bin/python"
  ;;
pip)
  case "$2" in
  compile) cat ;;
  esac
  ;;
esac
"#,
                version = mypypi::build::UV_VERSION,
            ),
        );
        let wheels = root.path().join("wheels");
        fs::create_dir(&wheels).unwrap();
        Workspace { fake: FakeS3::start_verified(), root, bin, wheels }
    }

    fn state_dir(&self) -> PathBuf {
        self.root.path().join("state/pypi")
    }

    fn wheel(&self, filename: &str, name: &str, version: &str, body: &str) -> PathBuf {
        let directory = self.wheels.join(format!("{name}-{version}-{body}"));
        fs::create_dir_all(&directory).unwrap();
        build_wheel(&directory, filename, name, version, body, false)
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        Command::new(env!("CARGO_BIN_EXE_stuff"))
            .args(args)
            .env("PATH", path)
            .env("PYTHON", self.bin.join("python"))
            .env("R2_S3_ENDPOINT", self.fake.base_url())
            .env("R2_S3_ACCESS_KEY_ID", ACCESS_KEY)
            .env("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY)
            .env("STUFF_CONFIG_HOME", self.root.path().join("config"))
            .env("STUFF_CACHE_HOME", self.root.path().join("cache"))
            .env("STUFF_STATE_HOME", self.root.path().join("state"))
            .output()
            .expect("run stuff")
    }

    fn publish_wheels(&self, wheels: &[&Path], extra: &[&str]) -> Output {
        let mut args: Vec<&str> = vec!["publish", "pypi"];
        for wheel in wheels {
            args.push("--wheel");
            args.push(wheel.to_str().unwrap());
        }
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn client(&self) -> R2Client {
        R2Client::new(Credentials {
            endpoint: self.fake.base_url().to_string(),
            access_key_id: ACCESS_KEY.to_string(),
            secret_access_key: SECRET_KEY.to_string(),
            region: "auto".to_string(),
            session_token: None,
        })
    }

    fn current(&self) -> String {
        let body = self.client().get_object(BUCKET, "current").expect("read current");
        String::from_utf8(body).unwrap().trim().to_string()
    }

    fn object(&self, key: &str) -> Vec<u8> {
        self.client().get_object(BUCKET, key).unwrap_or_else(|error| panic!("{key}: {error:?}"))
    }

    fn manifest(&self) -> Vec<(String, bool)> {
        let body = self.object(&format!("generations/{}/manifest.json", self.current()));
        let value: Value = deser_json::from_slice(&body).expect("manifest is JSON");
        value
            .as_map()
            .and_then(|map| map.get("artifacts"))
            .and_then(|artifacts| artifacts.as_seq())
            .expect("manifest artifacts")
            .iter()
            .map(|artifact| {
                let map = artifact.as_map().expect("manifest artifact");
                let filename = map.get("filename").and_then(|value| value.as_str()).unwrap().to_string();
                let first_party = map.get("first_party").is_some();
                (filename, first_party)
            })
            .collect()
    }

    fn seed_mirrored(&self) {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/mypypi/wheels")
            .join(MIRRORED_WHEEL);
        let cache = self.state_dir().join("wheels");
        fs::create_dir_all(&cache).unwrap();
        fs::copy(fixture, cache.join(MIRRORED_WHEEL)).unwrap();
    }

    fn requests_mutating(&self, since: usize) -> usize {
        self.fake.requests()[since..]
            .iter()
            .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
            .count()
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_exit(output: &Output, code: i32, needle: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(stderr.contains(needle), "missing {needle:?} in {stderr}");
}

fn sha256_of(path: &Path) -> String {
    mypypi::acquire::sha256_file(path).expect("hash a wheel")
}

fn tree_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(tree_files(&path));
            } else {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn a_first_party_wheel_is_served_with_its_hash_and_core_metadata() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    let output = work.publish_wheels(&[&wheel], &["--json"]);
    assert_success(&output);
    let value: Value = deser_json::from_slice(&output.stdout).expect("stdout is JSON");
    let map = value.as_map().expect("json object");
    assert_eq!(map.get("status").and_then(|v| v.as_str()), Some("published"));
    let packages: Vec<&str> = map
        .get("packages")
        .and_then(|v| v.as_seq())
        .expect("packages")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(packages, ["first-party-demo==0.1.0"]);

    let generation = work.current();
    assert_eq!(work.manifest(), vec![(FIRST_PARTY_WHEEL.to_string(), true)]);

    let sha256 = sha256_of(&wheel);
    let page = String::from_utf8(work.object(&format!("generations/{generation}/simple/first-party-demo/index.html"))).unwrap();
    assert!(
        page.contains(&format!("href=\"../../wheels/{FIRST_PARTY_WHEEL}#sha256={sha256}\"")),
        "{page}"
    );
    assert!(page.contains("data-requires-python=\"&gt;=3.15,&lt;3.16\""), "{page}");
    assert!(page.contains("data-core-metadata=\"sha256="), "{page}");
    let root = String::from_utf8(work.object(&format!("generations/{generation}/simple/index.html"))).unwrap();
    assert!(root.contains("first-party-demo"), "{root}");

    assert_eq!(work.object(&format!("wheels/{FIRST_PARTY_WHEEL}")), fs::read(&wheel).unwrap());
    let metadata = work.object(&format!("wheels/{FIRST_PARTY_WHEEL}.metadata"));
    assert!(String::from_utf8(metadata).unwrap().contains("Name: first-party-demo"));

    let wheels_in_state: Vec<PathBuf> = tree_files(&work.root.path().join("state"))
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "whl"))
        .collect();
    assert!(wheels_in_state.is_empty(), "first-party publication left wheel state: {wheels_in_state:?}");
}

#[test]
fn republishing_the_identical_wheel_is_a_no_op_even_without_local_state() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    assert_success(&work.publish_wheels(&[&wheel], &[]));
    let generation = work.current();
    fs::remove_dir_all(work.root.path().join("state")).unwrap();
    fs::remove_dir_all(work.root.path().join("cache")).unwrap();
    let requests = work.fake.requests().len();

    let output = work.publish_wheels(&[&wheel], &["--json"]);
    assert_success(&output);
    let value: Value = deser_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value.as_map().and_then(|m| m.get("status")).and_then(|v| v.as_str()), Some("unchanged"));
    assert_eq!(work.current(), generation);
    assert_eq!(work.requests_mutating(requests), 0);

    let republished = work.run(&["republish", "pypi"]);
    assert_success(&republished);
    assert_eq!(work.current(), generation);
    assert_eq!(work.manifest(), vec![(FIRST_PARTY_WHEEL.to_string(), true)]);
}

#[test]
fn different_bytes_for_a_published_version_conflict() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    assert_success(&work.publish_wheels(&[&wheel], &[]));
    let generation = work.current();
    let requests = work.fake.requests().len();

    let changed = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 2\n");
    assert_ne!(sha256_of(&changed), sha256_of(&wheel));
    assert_exit(&work.publish_wheels(&[&changed], &[]), 3, "different bytes");
    assert_eq!(work.current(), generation);
    assert_eq!(work.requests_mutating(requests), 0);

    let other_tag = work.wheel("first_party_demo-0.1.0-py2.py3-none-any.whl", "first-party-demo", "0.1.0", "A = 1\n");
    assert_exit(&work.publish_wheels(&[&other_tag], &[]), 3, "immutable");
    assert_eq!(work.current(), generation);

    let twice = work.wheel("first_party_demo-0.2.0-py3-none-any.whl", "first-party-demo", "0.2.0", "A = 1\n");
    let twice_changed = work.wheel("first_party_demo-0.2.0-py3-none-any.whl", "first-party-demo", "0.2.0", "A = 3\n");
    assert_exit(&work.publish_wheels(&[&twice, &twice_changed], &[]), 3, "twice with different bytes");
}

#[test]
fn a_first_party_name_cannot_reuse_a_mirrored_name() {
    let work = Workspace::new();
    work.seed_mirrored();
    assert_success(&work.run(&["publish", "pypi", "validation-fixture==1.0"]));
    let generation = work.current();
    let requests = work.fake.requests().len();

    let wheel = work.wheel("validation_fixture-2.0-py3-none-any.whl", "validation-fixture", "2.0", "A = 1\n");
    assert_exit(&work.publish_wheels(&[&wheel], &[]), 3, "hosted by the upstream mirror");
    assert_eq!(work.current(), generation);
    assert_eq!(work.requests_mutating(requests), 0);
}

#[test]
fn a_mirror_publish_carries_the_first_party_wheel_forward_and_cannot_add_its_versions() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    assert_success(&work.publish_wheels(&[&wheel], &[]));
    let first = work.current();
    let first_page = work.object(&format!("generations/{first}/simple/first-party-demo/index.html"));

    work.seed_mirrored();
    assert_success(&work.run(&["publish", "pypi", "validation-fixture==1.0"]));
    let second = work.current();
    assert_ne!(first, second);
    let mut manifest = work.manifest();
    manifest.sort();
    assert_eq!(
        manifest,
        vec![(FIRST_PARTY_WHEEL.to_string(), true), (MIRRORED_WHEEL.to_string(), false)]
    );
    assert_eq!(
        work.object(&format!("generations/{second}/simple/first-party-demo/index.html")),
        first_page
    );
    assert_eq!(work.object(&format!("wheels/{FIRST_PARTY_WHEEL}")), fs::read(&wheel).unwrap());

    let cache = work.state_dir().join("wheels");
    let other = work.wheel("first_party_demo-0.2.0-py3-none-any.whl", "first-party-demo", "0.2.0", "A = 2\n");
    fs::copy(&other, cache.join("first_party_demo-0.2.0-py3-none-any.whl")).unwrap();
    let output = work.run(&["publish", "pypi", "first-party-demo==0.2.0"]);
    assert_eq!(output.status.code(), Some(3), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(work.current(), second);
}

#[test]
fn platform_wheels_are_rejected() {
    let work = Workspace::new();
    for filename in [
        "plat_demo-1.0-cp315-cp315-macosx_11_0_arm64.whl",
        "plat_demo-1.0-py3-none-manylinux_2_28_x86_64.whl",
        "plat_demo-1.0-py3-abi3-any.whl",
    ] {
        let wheel = work.wheel(filename, "plat-demo", "1.0", "A = 1\n");
        let output = work.publish_wheels(&[&wheel], &[]);
        assert_exit(&output, 2, "none-any");
    }
    assert!(work.fake.state().is_empty());
}

#[test]
fn invalid_wheels_are_rejected_before_anything_is_uploaded() {
    let work = Workspace::new();
    let directory = work.wheels.join("tampered");
    fs::create_dir_all(&directory).unwrap();
    let tampered = build_wheel(&directory, FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n", true);
    assert_exit(&work.publish_wheels(&[&tampered], &[]), 2, "RECORD hash does not match");

    let mismatched = work.wheel("first_party_demo-0.2.0-py3-none-any.whl", "first-party-demo", "0.1.0", "A = 1\n");
    assert_exit(&work.publish_wheels(&[&mismatched], &[]), 2, "invalid wheel");

    let missing = work.wheels.join("missing-1.0-py3-none-any.whl");
    assert_exit(&work.publish_wheels(&[&missing], &[]), 2, "not a readable file");
    assert!(work.fake.state().is_empty());
}

#[test]
fn a_dry_run_changes_neither_the_remote_nor_local_state() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    assert_success(&work.publish_wheels(&[&wheel], &["--dry-run"]));
    assert!(work.fake.state().is_empty());
    assert_eq!(work.requests_mutating(0), 0);
}

#[test]
fn wheel_and_spec_arguments_do_not_combine() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    let path = wheel.to_str().unwrap();
    for args in [
        vec!["publish", "pypi", "--wheel", path, "other==1.0"],
        vec!["publish", "pypi", "--wheel", path, "--allow-sdist"],
        vec!["publish", "pypi", "--wheel"],
    ] {
        assert_eq!(work.run(&args).status.code(), Some(2), "{args:?}");
    }
}

fn seed_pulled_site(work: &Workspace, destination: &Path) {
    let generation = work.current();
    let client = work.client();
    for object in client.list_objects_v2(BUCKET).unwrap() {
        if !(object.key.starts_with(&format!("generations/{generation}/")) || object.key.starts_with("wheels/")) {
            continue;
        }
        let path = destination.join(&object.key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, client.get_object(BUCKET, &object.key).unwrap()).unwrap();
    }
    std::os::unix::fs::symlink(format!("generations/{generation}"), destination.join("current")).unwrap();
    std::os::unix::fs::symlink(
        "../../wheels",
        destination.join("generations").join(&generation).join("wheels"),
    )
    .unwrap();
}

struct StaticServer {
    child: Child,
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(directory: &Path, port: u16) -> StaticServer {
    let child = Command::new("python3")
        .arg("-I")
        .args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1", "--directory"])
        .arg(directory)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("serve the pulled site");
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    StaticServer { child }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

#[test]
fn uv_installs_the_first_party_wheel_from_the_generated_site() {
    let python = Command::new("uv").args(["python", "find", "3.15"]).output();
    let Some(python) = python.ok().filter(|output| output.status.success()) else {
        eprintln!("skipping: uv with a CPython 3.15 interpreter is unavailable");
        return;
    };
    let python = String::from_utf8(python.stdout).unwrap().trim().to_string();

    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    assert_success(&work.publish_wheels(&[&wheel], &[]));
    let site = work.root.path().join("site");
    seed_pulled_site(&work, &site);
    let port = free_port();
    let _server = serve(&site, port);

    let target = work.root.path().join("target");
    let output = Command::new("uv")
        .args(["pip", "install", "--no-cache", "--python", &python, "--target"])
        .arg(&target)
        .arg("--index-url")
        .arg(format!("http://127.0.0.1:{port}/current/simple/"))
        .arg("first-party-demo==0.1.0")
        .env("UV_NO_CONFIG", "1")
        .output()
        .expect("run uv pip install");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(target.join("first_party_demo/__init__.py")).unwrap(), "A = 1\n");
}

#[test]
fn the_store_rejects_a_mirrored_version_of_a_first_party_name() {
    let work = Workspace::new();
    let wheel = work.wheel(FIRST_PARTY_WHEEL, "first-party-demo", "0.1.0", "A = 1\n");
    let site = work.root.path().join("direct");
    let identity = WheelIdentity::parse(FIRST_PARTY_WHEEL).unwrap();
    let targets = vec![Target::new("3.15", "macos-arm64").unwrap()];
    let artifact =
        ValidatedArtifact::new(identity, wheel.clone(), sha256_of(&wheel), targets.clone()).unwrap();
    let url = &Config::default().pypi.public_base_url;
    mypypi::store::publish(
        &[artifact.clone().into_first_party()],
        &site,
        &rustc_hash::FxHashMap::default(),
        url,
    )
    .unwrap();

    let other = work.wheel("first_party_demo-0.2.0-py3-none-any.whl", "first-party-demo", "0.2.0", "A = 2\n");
    let mirrored = ValidatedArtifact::new(
        WheelIdentity::parse("first_party_demo-0.2.0-py3-none-any.whl").unwrap(),
        other.clone(),
        sha256_of(&other),
        targets,
    )
    .unwrap();
    let error = mypypi::store::publish(&[mirrored], &site, &rustc_hash::FxHashMap::default(), url).unwrap_err();
    assert!(matches!(error.kind, mypypi::error::Kind::Conflict), "{error}");
}
