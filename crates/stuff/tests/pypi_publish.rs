#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use deser_value::Value;
use stuff::publish::config::Config;
use stuff::publish::pypi::{package_catalog, project_settings};
use stuff::publish::r2::{Credentials, R2Client};
use stuff::publish::PypiSpec;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mypypi";
const FIXTURE_WHEEL: &str = "validation_fixture-1.0-py3-none-any.whl";

fn fixture_wheel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/mypypi/wheels")
        .join(FIXTURE_WHEEL)
}

fn spec() -> PypiSpec {
    PypiSpec {
        name: "validation-fixture".to_string(),
        version: "1.0".to_string(),
    }
}

struct Workspace {
    fake: FakeS3,
    root: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let wheels = root.path().join("state/pypi/wheels");
        fs::create_dir_all(&wheels).unwrap();
        fs::copy(fixture_wheel(), wheels.join(FIXTURE_WHEEL)).unwrap();
        Workspace {
            fake: FakeS3::start_verified(),
            root,
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
        Command::new(env!("CARGO_BIN_EXE_stuff"))
            .args(args)
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
fn first_publish_matches_the_direct_mypypi_generation() {
    let work = Workspace::new();
    assert_success(&work.publish());
    let id = work.current();
    assert!(!id.is_empty());

    let settings = project_settings(&Config::default().pypi.public_base_url).unwrap();
    let catalog = package_catalog(&[spec()]).unwrap();
    let oracle = work.root.path().join("oracle");
    let summary = mypypi::publish::publish(&mypypi::publish::PublishOptions {
        settings: &settings,
        output: &oracle,
        state_dir: &work.state_dir(),
        catalog: &catalog,
        platforms: &[],
    })
    .unwrap();
    assert!(!summary.no_op);
    assert_eq!(summary.generation_id, id);

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
