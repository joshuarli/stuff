mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use stuff::publish::oci::OciPaths;
use stuff::publish::r2::{Credentials, R2Client};
use stuff::publish::{OciSource, Options, PublishRequest, Request, Status};
use stuff_oci::RepositoryName;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const REPOSITORY: &str = "example/app";
const METADATA_BUCKET: &str = "stuff-oci";
const BLOB_BUCKET: &str = "stuff-oci-blobs";

fn layout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/oci/layout")
}

fn configure(fake: &FakeS3, home: &Path) {
    unsafe {
        std::env::set_var("R2_S3_ENDPOINT", fake.base_url());
        std::env::set_var("R2_S3_ACCESS_KEY_ID", ACCESS_KEY);
        std::env::set_var("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY);
        std::env::remove_var("R2_S3_REGION");
        std::env::remove_var("R2_S3_SESSION_TOKEN");
        std::env::set_var("STUFF_STATE_HOME", home.join("state"));
        std::env::set_var("STUFF_CACHE_HOME", home.join("cache"));
        std::env::set_var("STUFF_CONFIG_HOME", home.join("config"));
    }
}

fn request() -> Request {
    Request::Publish {
        request: PublishRequest::Oci(OciSource::Layout {
            dir: layout().to_string_lossy().into_owned(),
            repository: REPOSITORY.to_string(),
            tag: None,
        }),
        options: Options::default(),
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

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(root, root, &mut out);
    out
}

fn collect(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
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
    configure(&fake, &home);
    let paths = OciPaths::from_env(|key| std::env::var(key).ok());

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

    let outcome = stuff::publish::run(request()).expect("publish oci");
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
    let second = stuff::publish::run(request()).expect("second publish");
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
