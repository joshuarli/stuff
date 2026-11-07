#![forbid(unsafe_code)]

mod support;

use std::path::PathBuf;

use stuff::publish::PublishError;
use stuff::publish::r2::put::{PutAction, UploadOptions};
use stuff::publish::r2::sync::{Payload, PlanEntry, SitePlan, execute_with_concurrency};
use stuff::publish::r2::{Credentials, R2Client};
use support::{ACCESS_KEY, FakeS3, SECRET_KEY};

const BUCKET: &str = "bucket";
const MANIFEST: &str = "manifests/g.json";

fn client(fake: &FakeS3) -> R2Client {
    R2Client::new(Credentials {
        endpoint: fake.base_url().to_string(),
        access_key_id: ACCESS_KEY.to_string(),
        secret_access_key: SECRET_KEY.to_string(),
        region: "auto".to_string(),
        session_token: None,
    })
}

fn object_entry(index: usize) -> PlanEntry {
    let key = format!("objects/o{index:03}.json");
    PlanEntry {
        key: key.clone(),
        size: 9,
        action: PutAction::Put,
        payload: Payload::File(PathBuf::from(key)),
    }
}

fn manifest_entry() -> PlanEntry {
    PlanEntry {
        key: MANIFEST.to_string(),
        size: 2,
        action: PutAction::Put,
        payload: Payload::Memory(b"{}".to_vec()),
    }
}

fn site(count: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("site dir");
    std::fs::create_dir_all(dir.path().join("objects")).expect("objects dir");
    for index in 0..count {
        std::fs::write(
            dir.path().join(format!("objects/o{index:03}.json")),
            b"{\"n\":0}\n",
        )
        .expect("object file");
    }
    dir
}

fn plan(entries: Vec<PlanEntry>) -> SitePlan {
    SitePlan {
        generation: "g".to_string(),
        entries,
        aliases: Vec::new(),
    }
}

fn put_paths(fake: &FakeS3) -> Vec<String> {
    fake.requests()
        .into_iter()
        .filter(|request| request.method == "PUT")
        .map(|request| request.path)
        .collect()
}

#[test]
fn the_manifest_is_uploaded_after_every_object_under_concurrency() {
    let fake = FakeS3::start_verified();
    let site = site(40);
    let mut entries: Vec<PlanEntry> = (0..40).map(object_entry).collect();
    entries.push(manifest_entry());
    entries.sort_by(|left, right| left.key.cmp(&right.key));
    let report = execute_with_concurrency(
        &client(&fake),
        site.path(),
        BUCKET,
        &plan(entries),
        &UploadOptions::default(),
        8,
    )
    .expect("upload succeeds");

    let paths = put_paths(&fake);
    assert_eq!(paths.len(), 41);
    assert_eq!(paths.last().map(String::as_str), Some("/bucket/manifests/g.json"));
    assert_eq!(report.uploaded_objects(), 41);
    let mut expected: Vec<String> = (0..40).map(|i| format!("objects/o{i:03}.json")).collect();
    expected.push(MANIFEST.to_string());
    assert_eq!(report.uploaded, expected);
}

#[test]
fn the_first_failure_returns_and_keeps_the_manifest_from_uploading() {
    let fake = FakeS3::start_verified();
    let site = site(40);
    std::fs::remove_file(site.path().join("objects/o002.json")).expect("remove an object");
    let mut entries: Vec<PlanEntry> = (0..40).map(object_entry).collect();
    entries.push(manifest_entry());
    entries.sort_by(|left, right| left.key.cmp(&right.key));
    let error = execute_with_concurrency(
        &client(&fake),
        site.path(),
        BUCKET,
        &plan(entries),
        &UploadOptions::default(),
        4,
    )
    .expect_err("a missing object fails the upload");

    assert!(
        matches!(&error, PublishError::Integrity(text) if text.contains("o002.json")),
        "{error}"
    );
    let paths = put_paths(&fake);
    assert!(!paths.iter().any(|path| path.ends_with("manifests/g.json")));
    assert!(paths.len() < 40, "{} puts after the failure", paths.len());
}
