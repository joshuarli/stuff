#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use deser_value::Value;
use mypypi::models::{Target, ValidatedArtifact, WheelIdentity};
use stuff::publish::config::Config;
use stuff::publish::pypi::project_settings;
use stuff::publish::r2::{Credentials, R2Client};
use support::{seed_pypi_generation, FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mypypi";
const FIXTURE_WHEEL: &str = "validation_fixture-1.0-py3-none-any.whl";
const RETAINED_WHEEL: &str = "retained_fixture-1.0-py3-none-any.whl";

const FAKE_PYTHON: &str = r#"#!/bin/sh
case "$*" in
*implementation.name*) echo 'cpython 3 15' ;;
*version_info.micro*) echo '3 15 1' ;;
*sys.executable*) echo /fake/python ;;
*cache_tag*) printf 'cpython\n3.15.1\ncpython-315\nfake-platform\n' ;;
esac
"#;

fn fixture_wheel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/mypypi/wheels")
        .join(FIXTURE_WHEEL)
}

fn write_retained_wheel(path: &Path) {
    let script = r#"
import sys, zipfile
path = sys.argv[1]
dist = "retained_fixture-1.0.dist-info"
metadata = "Metadata-Version: 2.4\nName: retained-fixture\nVersion: 1.0\nRequires-Python: >=3.15,<3.16\n\n"
wheel = "Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
record = f"{dist}/METADATA,,\n{dist}/WHEEL,,\n{dist}/RECORD,,\n"
with zipfile.ZipFile(path, "w") as archive:
    archive.writestr(f"{dist}/METADATA", metadata)
    archive.writestr(f"{dist}/WHEEL", wheel)
    archive.writestr(f"{dist}/RECORD", record)
"#;
    let status = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()
        .expect("run python3 to build the retained wheel");
    assert!(status.success(), "python3 could not build {RETAINED_WHEEL}");
}

fn seed_live_generation(fake: &FakeS3, wheel: &Path, targets: Vec<Target>) -> String {
    let root = tempfile::tempdir().unwrap();
    let site = root.path().join("site");
    let identity = WheelIdentity::parse(
        wheel
            .file_name()
            .and_then(|name| name.to_str())
            .expect("wheel filename"),
    )
    .expect("retained wheel filename");
    let sha256 = mypypi::acquire::sha256_file(wheel).expect("hash the retained wheel");
    let artifact = ValidatedArtifact::new(identity, wheel.to_path_buf(), sha256, targets)
        .expect("validated retained artifact");
    let (_, activation) = mypypi::store::publish(
        &[artifact],
        &site,
        &std::collections::BTreeMap::new(),
        &Config::default().pypi.public_base_url,
    )
    .expect("seed the live pypi generation");
    for (key, body) in tree(&site) {
        fake.seed(BUCKET, &key, &body, "application/octet-stream");
    }
    fake.seed(
        BUCKET,
        "current",
        activation.generation_id.as_bytes(),
        "text/plain",
    );
    activation.generation_id
}

fn manifest_targets(work: &Workspace, generation: &str) -> Vec<(String, Vec<String>)> {
    let body = work
        .client()
        .get_object(BUCKET, &format!("generations/{generation}/manifest.json"))
        .expect("promoted generation manifest");
    let value: Value = deser_json::from_slice(&body).expect("manifest is JSON");
    value
        .as_map()
        .and_then(|map| map.get("artifacts"))
        .and_then(|artifacts| artifacts.as_seq())
        .expect("manifest artifacts")
        .iter()
        .map(|artifact| {
            let map = artifact.as_map().expect("manifest artifact");
            let filename = map
                .get("filename")
                .and_then(|value| value.as_str())
                .expect("artifact filename")
                .to_string();
            let targets = map
                .get("targets")
                .and_then(|value| value.as_seq())
                .expect("artifact targets")
                .iter()
                .map(|target| target.as_str().expect("target string").to_string())
                .collect();
            (filename, targets)
        })
        .collect()
}

struct Workspace {
    fake: FakeS3,
    root: tempfile::TempDir,
    bin: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let python = bin.join("python");
        fs::write(&python, FAKE_PYTHON).unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
        let uv = bin.join("uv");
        fs::write(
            &uv,
            format!(
                r#"#!/bin/sh
echo "$*" >> "{log}"
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
                log = root.path().join("uv.log").display(),
                version = mypypi::build::UV_VERSION,
            ),
        )
        .unwrap();
        fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
        let wheels = root.path().join("state/pypi/wheels");
        fs::create_dir_all(&wheels).unwrap();
        fs::copy(fixture_wheel(), wheels.join(FIXTURE_WHEEL)).unwrap();
        Workspace {
            fake: FakeS3::start_verified(),
            root,
            bin,
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.root.path().join("state/pypi")
    }

    fn publish(&self) -> Output {
        self.publish_args(&["publish", "pypi", "validation-fixture==1.0"])
    }

    fn dry_run(&self) -> Output {
        self.publish_args(&["publish", "pypi", "validation-fixture==1.0", "--dry-run"])
    }

    fn publish_args(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
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
            .expect("run stuff publish pypi")
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
        let body = self
            .client()
            .get_object(BUCKET, "current")
            .expect("read current");
        String::from_utf8(body).unwrap().trim().to_string()
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

fn tree(directory: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, relative: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(root.join(relative))
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let child = relative.join(entry.file_name());
            let metadata = entry.file_type().unwrap();
            if metadata.is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                walk(root, &child, files);
                continue;
            }
            let name = child
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push((name, fs::read(entry.path()).unwrap()));
        }
    }
    let mut files = Vec::new();
    walk(directory, Path::new(""), &mut files);
    files
}

fn object_snapshot(fake: &FakeS3) -> Vec<(String, String, u64, String)> {
    let mut objects: Vec<_> = fake
        .state()
        .into_iter()
        .map(|object| (object.bucket, object.key, object.size, object.etag))
        .collect();
    objects.sort();
    objects
}

#[test]
fn a_spec_absent_from_live_and_state_is_added_validated_and_published() {
    let work = Workspace::new();
    assert!(
        work.fake.state().is_empty(),
        "the test started with a live index"
    );

    assert_success(&work.publish());
    let id = work.current();
    assert!(!id.is_empty());

    let manifest = work
        .client()
        .get_object(BUCKET, &format!("generations/{id}/manifest.json"))
        .expect("generation manifest");
    let text = String::from_utf8(manifest).unwrap();
    assert!(text.contains("validation-fixture"), "{text}");

    let page = work
        .client()
        .get_object(BUCKET, &format!("generations/{id}/simple/validation-fixture/index.html"))
        .expect("project page");
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains(FIXTURE_WHEEL), "{page}");

    let log = fs::read_to_string(work.root.path().join("uv.log")).unwrap();
    assert!(log.contains("pip compile"), "the add step never ran: {log}");
    assert!(
        log.contains("pip install") && log.contains("--no-index"),
        "the validate step never ran: {log}"
    );
}

#[test]
fn a_live_wheel_for_another_platform_does_not_block_the_local_publish() {
    let Some(local) = mypypi::validate::local_platform() else {
        return;
    };
    let non_local = if local.starts_with("macos") {
        "linux-aarch64"
    } else {
        "macos-arm64"
    };

    let work = Workspace::new();
    let retained = tempfile::tempdir().unwrap();
    let retained_wheel = retained.path().join(RETAINED_WHEEL);
    write_retained_wheel(&retained_wheel);
    let live = seed_live_generation(
        &work.fake,
        &retained_wheel,
        vec![Target::new("3.15", non_local).unwrap()],
    );

    assert_success(&work.publish());

    let promoted = work.current();
    assert_ne!(promoted, live, "the publish promoted no new generation");
    let artifacts = manifest_targets(&work, &promoted);

    let retained_targets = &artifacts
        .iter()
        .find(|(filename, _)| filename == RETAINED_WHEEL)
        .unwrap_or_else(|| panic!("the retained wheel was dropped: {artifacts:?}"))
        .1;
    assert_eq!(
        retained_targets,
        &vec![format!("3.15:{non_local}")],
        "{artifacts:?}"
    );

    let new_targets = &artifacts
        .iter()
        .find(|(filename, _)| filename == FIXTURE_WHEEL)
        .unwrap_or_else(|| panic!("the new wheel was not published: {artifacts:?}"))
        .1;
    assert_eq!(
        new_targets,
        &vec![format!("3.15:{local}")],
        "{artifacts:?}"
    );

    work.client()
        .get_object(BUCKET, &format!("wheels/{RETAINED_WHEEL}"))
        .expect("the retained wheel object is still in the bucket");
}

#[test]
fn first_publish_matches_the_direct_mypypi_generation() {
    let work = Workspace::new();
    assert_success(&work.publish());
    let id = work.current();
    assert!(!id.is_empty());

    let settings = project_settings(&Config::default().pypi.public_base_url).unwrap();
    let local = mypypi::validate::local_platform().unwrap();
    let local_targets: Vec<_> = settings
        .targets()
        .into_iter()
        .filter(|target| target.platform == local)
        .collect();
    let oracle = work.root.path().join("oracle");
    let (oracle_id, no_op) =
        seed_pypi_generation(&fixture_wheel(), local_targets, &oracle, &settings.public_base_url);
    assert!(!no_op);
    assert_eq!(oracle_id, id);

    let generation = oracle.join("generations").join(&id);
    let files = tree(&generation);
    assert!(files.len() >= 4, "{:?}", files.iter().map(|(n, _)| n).collect::<Vec<_>>());
    let client = work.client();
    for (relative, bytes) in &files {
        let key = format!("generations/{id}/{relative}");
        let remote = client.get_object(BUCKET, &key).expect(&key);
        assert_eq!(&remote, bytes, "{key}");
    }

    let prefix = format!("generations/{id}/");
    let mut remote_names: Vec<String> = client
        .list_objects_v2(BUCKET)
        .unwrap()
        .into_iter()
        .filter_map(|object| object.key.strip_prefix(&prefix).map(str::to_string))
        .collect();
    remote_names.sort();
    let mut local_names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    local_names.sort();
    assert_eq!(remote_names, local_names);

    let wheel = fs::read(fixture_wheel()).unwrap();
    let remote = client
        .get_object(BUCKET, &format!("wheels/{FIXTURE_WHEEL}"))
        .expect("uploaded wheel");
    assert_eq!(remote, wheel);
}

#[test]
fn a_publish_from_the_pulled_generation_is_unchanged_and_promotes_nothing() {
    let work = Workspace::new();
    assert_success(&work.publish());
    let id = work.current();
    assert!(fs::remove_file(work.state_dir().join("wheels").join(FIXTURE_WHEEL)).is_ok());
    let objects = object_snapshot(&work.fake);
    let requests = work.fake.requests().len();
    let conditions = work.fake.put_conditions().len();
    assert_eq!(conditions, 1, "the first publication is conditional");

    assert_success(&work.publish());
    assert_eq!(work.current(), id);
    assert_eq!(object_snapshot(&work.fake), objects);
    assert_eq!(work.fake.put_conditions().len(), conditions);

    let later = work.fake.requests();
    let mutations = later[requests..]
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count();
    assert_eq!(
        mutations,
        0,
        "{:?}",
        later[requests..]
            .iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect::<Vec<_>>()
    );
    let manifest = format!("/{BUCKET}/generations/{id}/manifest.json");
    assert!(
        later[requests..]
            .iter()
            .any(|request| request.method == "GET" && request.path == manifest),
        "the repeated publish did not pull the live generation"
    );
}

#[test]
fn json_output_reports_the_requested_package() {
    let work = Workspace::new();
    let output = work.publish_args(&["publish", "pypi", "validation-fixture==1.0", "--json"]);
    assert_success(&output);
    let value: Value = deser_json::from_slice(&output.stdout).expect("stdout is JSON");
    let packages: Vec<String> = value
        .as_map()
        .and_then(|map| map.get("packages"))
        .and_then(|packages| packages.as_seq())
        .expect("packages list")
        .iter()
        .map(|entry| entry.as_str().expect("package string").to_string())
        .collect();
    assert_eq!(packages, vec!["validation-fixture==1.0".to_string()]);
}

#[test]
fn a_fresh_cache_publish_reuses_stored_wheels_instead_of_downloading_them() {
    let work = Workspace::new();
    assert_success(&work.publish());
    let generation = work.current();
    let remote_before = object_snapshot(&work.fake);
    let manifest_before = work
        .client()
        .get_object(BUCKET, &format!("generations/{generation}/manifest.json"))
        .expect("generation manifest");

    fs::remove_dir_all(work.root.path().join("cache")).unwrap();
    let requests = work.fake.requests().len();
    assert_success(&work.publish());
    assert_eq!(work.current(), generation);
    assert_eq!(
        object_snapshot(&work.fake),
        remote_before,
        "the repeated publish changed the remote"
    );
    assert_eq!(
        work.client()
            .get_object(BUCKET, &format!("generations/{generation}/manifest.json"))
            .expect("generation manifest"),
        manifest_before,
        "the generation manifest bytes changed"
    );
    let wheel_path = format!("/{BUCKET}/wheels/{FIXTURE_WHEEL}");
    let downloads: Vec<String> = work.fake.requests()[requests..]
        .iter()
        .filter(|request| request.method == "GET" && request.path == wheel_path)
        .map(|request| request.path.clone())
        .collect();
    assert!(
        downloads.is_empty(),
        "the fresh cache downloaded wheels already stored locally: {downloads:?}"
    );
}

#[test]
fn a_pypi_dry_run_leaves_local_state_and_remote_unchanged() {
    let work = Workspace::new();
    assert_success(&work.publish());

    let state_before = tree(&work.root.path().join("state"));
    let remote_before = object_snapshot(&work.fake);
    let requests = work.fake.requests().len();
    let conditions = work.fake.put_conditions().len();
    assert!(!state_before.is_empty(), "the first publish wrote no local state");

    assert_success(&work.dry_run());

    assert_eq!(
        tree(&work.root.path().join("state")),
        state_before,
        "the dry run changed local state"
    );
    assert_eq!(object_snapshot(&work.fake), remote_before, "the dry run changed the remote");
    assert_eq!(work.fake.put_conditions().len(), conditions);
    let mutations = work.fake.requests()[requests..]
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count();
    assert_eq!(mutations, 0, "the dry run mutated the remote");
}
