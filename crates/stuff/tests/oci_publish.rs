#![forbid(unsafe_code)]

mod support;

use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

use stuff::publish::oci::OciPaths;
use stuff::publish::r2::{Credentials, R2Client};
use stuff::publish::{Environment, OciSource, Options, PublishRequest, Registry, Request, Status};
use stuff_oci::RepositoryName;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const REPOSITORY: &str = "example/app";
const METADATA_BUCKET: &str = "stuff-oci";
const BLOB_BUCKET: &str = "stuff-oci-blobs";

fn layout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oci/layout")
}

fn environment(fake: &FakeS3, home: &Path) -> Environment {
    let mut pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| !name.starts_with("STUFF_") && !name.starts_with("R2_"))
        .collect();
    for (name, value) in [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_STATE_HOME", home.join("state").display().to_string()),
        ("STUFF_CACHE_HOME", home.join("cache").display().to_string()),
        ("STUFF_CONFIG_HOME", home.join("config").display().to_string()),
    ] {
        pairs.push((name.to_string(), value));
    }
    Environment::from_pairs(pairs)
}

fn request(dry_run: bool) -> Request {
    Request::Publish {
        request: PublishRequest::Oci(OciSource::Layout {
            dir: layout().to_string_lossy().into_owned(),
            repository: REPOSITORY.to_string(),
            tag: None,
        }),
        options: Options {
            dry_run,
            ..Options::default()
        },
    }
}

fn client(fake: &FakeS3) -> R2Client {
    R2Client::new(Credentials {
        endpoint: fake.base_url().to_string(),
        access_key_id: ACCESS_KEY.to_string(),
        secret_access_key: SECRET_KEY.to_string(),
        region: "auto".to_string(),
        session_token: None,
    })
}

fn current_generation(site: &Path) -> String {
    let target = std::fs::read_link(site.join("current")).expect("current symlink");
    let text = target.to_string_lossy();
    text.strip_prefix("generations/")
        .unwrap_or(&text)
        .to_string()
}

fn tree(root: &Path) -> FxHashMap<String, Vec<u8>> {
    let mut out = FxHashMap::default();
    collect(root, root, &mut out);
    out
}

fn collect(dir: &Path, root: &Path, out: &mut FxHashMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file type");
        if file_type.is_dir() {
            collect(&path, root, out);
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("relative")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(relative, std::fs::read(&path).expect("read file"));
        }
    }
}

fn mutation_count(fake: &FakeS3) -> usize {
    fake.requests()
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count()
}

#[test]
fn the_fixture_publishes_like_myoci_and_the_second_run_is_unchanged() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().expect("temp dir");
    let home = work.path().join("home");
    let environment = environment(&fake, &home);
    let paths = OciPaths::from_environment(&environment);

    let direct_site = work.path().join("direct-site");
    let direct_state = work.path().join("direct-state");
    let state = myoci::state::State::new(direct_state);
    let repository = RepositoryName::parse(REPOSITORY).expect("repository");
    myoci::import::import_layout(&state, &layout(), &repository, None).expect("direct import");
    let direct = myoci::publish::publish(
        &state,
        &direct_site,
        myoci::publish::DEFAULT_PUBLIC_URL,
    )
    .expect("direct publish");

    let outcome = stuff::publish::run(request(false), &environment).expect("publish oci");
    assert_eq!(outcome.status, Status::Published);

    let generation = current_generation(&paths.site_dir);
    assert_eq!(generation, direct.generation);
    assert_eq!(
        tree(&paths.site_dir.join("generations").join(&generation)),
        tree(&direct_site.join("generations").join(&direct.generation))
    );

    let objects = fake.state();
    let blobs: Vec<&str> = objects
        .iter()
        .filter(|object| object.bucket == BLOB_BUCKET)
        .map(|object| object.key.as_str())
        .collect();
    assert_eq!(blobs.len(), 6, "{blobs:?}");
    assert!(blobs.iter().all(|key| key.starts_with("sha256/")), "{blobs:?}");
    assert!(
        objects
            .iter()
            .filter(|object| object.bucket == METADATA_BUCKET)
            .all(|object| !object.key.starts_with("blobs/"))
    );
    assert!(objects.iter().any(|object| {
        object.bucket == METADATA_BUCKET
            && object.key == format!("generations/{generation}/catalog.json")
    }));
    assert!(objects.iter().any(|object| {
        object.bucket == METADATA_BUCKET && object.key == format!("manifests/{generation}.json")
    }));

    let writes = mutation_count(&fake);
    let second = stuff::publish::run(request(false), &environment).expect("second publish");
    assert_eq!(second.status, Status::Unchanged);
    assert_eq!(mutation_count(&fake), writes);
    assert_eq!(fake.put_conditions().len(), 1);
    assert_eq!(
        client(&fake)
            .get_object(METADATA_BUCKET, "current")
            .expect("current"),
        generation.as_bytes()
    );
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

#[test]
fn an_oci_dry_run_leaves_local_state_and_remote_unchanged() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().expect("temp dir");
    let home = work.path().join("home");
    let environment = environment(&fake, &home);

    let first = stuff::publish::run(request(false), &environment).expect("publish oci");
    assert_eq!(first.status, Status::Published);

    let local_before = tree(&home.join("state"));
    let remote_before = remote_snapshot(&fake);
    let requests_before = fake.requests().len();
    let conditions_before = fake.put_conditions().len();
    assert!(!local_before.is_empty(), "the first publish wrote no local state");

    let dry = stuff::publish::run(request(true), &environment).expect("dry run oci");
    assert_eq!(dry.registry, Registry::Oci);

    assert_eq!(tree(&home.join("state")), local_before, "the dry run changed local state");
    assert_eq!(remote_snapshot(&fake), remote_before, "the dry run changed the remote");
    assert_eq!(fake.put_conditions().len(), conditions_before);
    let mutations = fake.requests()[requests_before..]
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count();
    assert_eq!(mutations, 0, "the dry run mutated the remote");
}

#[test]
fn oci_bootstrap_reuses_cached_layers_without_redownload() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().expect("temp dir");
    let home = work.path().join("home");
    let environment = environment(&fake, &home);
    let paths = OciPaths::from_environment(&environment);

    let outcome = stuff::publish::run(request(false), &environment).expect("first publish");
    assert_eq!(outcome.status, Status::Published);

    let _ = std::fs::remove_dir_all(&paths.site_dir);

    let gets_before = fake.requests().iter().filter(|r| r.method == "GET").count();

    let second = stuff::publish::run(request(false), &environment).expect("second publish");
    assert_eq!(second.status, Status::Unchanged);

    let blob_gets = fake.requests()[gets_before..]
        .iter()
        .filter(|r| r.method == "GET" && r.path.starts_with(&format!("/{BLOB_BUCKET}/")))
        .count();
    assert_eq!(blob_gets, 0, "expected 0 blob GETs because layers were cached");
}

#[test]
fn the_mirror_mode_publishes_with_provenance_and_replays_unchanged() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().expect("temp dir");
    let home = work.path().join("home");
    let environment = environment(&fake, &home);
    let paths = OciPaths::from_environment(&environment);

    // Serve the fixture layout as a loopback OCI registry for the mirror to resolve.
    let serve_state_dir = work.path().join("serve-state");
    let serve_state = myoci::state::State::new(serve_state_dir);
    let fixture_repo = stuff_oci::RepositoryName::parse("example/app").expect("repository");
    myoci::import::import_layout(&serve_state, &layout(), &fixture_repo, None).expect("import layout");
    let serve_site = work.path().join("serve-site");
    myoci::publish::publish(&serve_state, &serve_site, myoci::publish::DEFAULT_PUBLIC_URL)
        .expect("publish fixture site");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral port")
        .port();
    std::thread::spawn(move || {
        let _ = myoci::serve::run(myoci::serve::ServeOptions {
            site_dir: serve_site,
            bind: format!("127.0.0.1:{port}"),
            max_connections: 16,
            push: None,
        });
    });
    let mut served = false;
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            served = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(served, "the loopback registry did not start");

    // Resolve the mirror inventory against the fixture registry.
    let inventory_path = work.path().join("mirror.toml");
    std::fs::write(
        &inventory_path,
        format!(
            "schema_version = 1\npolicy_version = 1\n\n[[entries]]\nid = \"sim\"\nsource = \"127.0.0.1:{port}/example/app:stable\"\ndestination = \"example/app\"\naliases = [\"pinned\"]\nplatforms = [\"linux/amd64\"]\nreferrers = \"none\"\n"
        ),
    )
    .expect("write inventory");
    let lock_path = work.path().join("mirror.lock");
    myoci::mirror::resolve::run(&myoci::mirror::MirrorOptions {
        command: myoci::mirror::MirrorCommand::Resolve,
        config: inventory_path.to_string_lossy().into_owned(),
        lock: lock_path.to_string_lossy().into_owned(),
        state_dir: None,
        locked: false,
        json: false,
        offline: false,
        entries: vec![],
    })
    .expect("resolve mirror lock");

    let request = Request::Publish {
        request: PublishRequest::Oci(OciSource::Mirror {
            inventory: inventory_path.to_string_lossy().into_owned(),
            lock: lock_path.to_string_lossy().into_owned(),
        }),
        options: Options::default(),
    };
    let outcome = stuff::publish::run(request.clone(), &environment).expect("mirror publish");
    assert_eq!(outcome.status, Status::Published);

    let generation = current_generation(&paths.site_dir);
    let objects = fake.state();
    assert!(
        objects.iter().any(|object| {
            object.bucket == METADATA_BUCKET && object.key == format!("generations/{generation}/provenance.json")
        }),
        "the published generation has no provenance sidecar remotely",
    );
    let repository_json = std::fs::read_to_string(paths.state_dir.join("repositories/docker.io/example/app/repository.json"))
        .expect("read adopted repository");
    assert!(repository_json.contains("\"pinned\""), "{repository_json}");
    assert!(repository_json.contains("\"pinned\""), "{repository_json}");
    let sidecar = std::fs::read_to_string(paths.state_dir.join("provenance.json")).expect("read adopted provenance");
    assert!(sidecar.contains("\"id\":\"sim\""), "{sidecar}");
    myoci::verify::verify_state(&myoci::state::State::new(paths.state_dir.clone()))
        .expect("adopted state does not verify");

    let second = stuff::publish::run(request, &environment).expect("replay");
    assert_eq!(second.status, Status::Unchanged);
}
