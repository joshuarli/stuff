#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use stuff::publish::config::Config;
use stuff::publish::r2::promote;
use stuff::publish::r2::pull::md5_hex;
use stuff::publish::r2::sync;
use stuff::publish::r2::{Credentials, R2Client, UploadOptions};
use stuff::publish::transaction::{self, Transaction, TransactionOptions};
use stuff::publish::{PublishError, Registry, Status};
use stuff_store::FileLock;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const BUCKET: &str = "stuff-mycrates";
const PKG_A: &str =
    r#"{"name":"a","source":"native","latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}]}"#;
const PKG_A_YANKED: &str =
    r#"{"name":"a","source":"native","latest":"1.0.0","versions":[{"version":"1.0.0","yanked":true}]}"#;
const PKG_B: &str =
    r#"{"name":"b","source":"upstream","latest":"2.0.0","versions":[{"version":"2.0.0","yanked":false}]}"#;

fn credentials(fake: &FakeS3) -> Credentials {
    Credentials {
        endpoint: fake.base_url().to_string(),
        access_key_id: ACCESS_KEY.to_string(),
        secret_access_key: SECRET_KEY.to_string(),
        region: "auto".to_string(),
        session_token: None,
    }
}

fn client(fake: &FakeS3) -> R2Client {
    R2Client::new(credentials(fake))
}

fn options() -> TransactionOptions {
    TransactionOptions {
        list_page: 50,
        attempts: 1,
        ..TransactionOptions::default()
    }
}

fn transaction(
    fake: &FakeS3,
    site: &Path,
    state: &Path,
    cache: &Path,
    options: TransactionOptions,
) -> Result<Transaction, PublishError> {
    let pending = Transaction::begin(
        credentials(fake),
        Registry::Cargo,
        &Config::default(),
        state.to_path_buf(),
        cache.to_path_buf(),
        options,
    )?;
    pending.stage_complete(site.to_path_buf())
}

fn catalog(generation: &str, packages: &[&str]) -> String {
    format!(
        "{{\"schema_version\":3,\"registry\":\"cargo\",\"generation\":\"{generation}\",\"packages\":[{}]}}\n",
        packages.join(",")
    )
}

fn add_generation(site: &Path, generation: &str, packages: &[&str]) {
    let directory = site.join("generations").join(generation);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("catalog.json"), catalog(generation, packages)).unwrap();
    if !site.join("generations").join(generation).join("tarballs").is_dir() {
        fs::create_dir_all(directory.join("tarballs")).unwrap();
        fs::write(directory.join("tarballs/alias.tgz"), b"alias\n").unwrap();
        fs::create_dir_all(directory.join("-/tarballs")).unwrap();
        fs::write(directory.join("-/tarballs/alias.tgz"), b"alias\n").unwrap();
    }
}

fn set_current(site: &Path, generation: &str) {
    let current = site.join("current");
    if current.symlink_metadata().is_ok() {
        fs::remove_file(&current).unwrap();
    }
    std::os::unix::fs::symlink(format!("generations/{generation}"), current).unwrap();
}

fn base_site(root: &Path, objects: usize) -> PathBuf {
    let site = root.join("site");
    add_generation(&site, "g1", &[PKG_A]);
    fs::create_dir_all(site.join("tarballs")).unwrap();
    for index in 0..objects {
        fs::write(
            site.join(format!("tarballs/pkg-{index}.tgz")),
            format!("object {index}\n"),
        )
        .unwrap();
    }
    set_current(&site, "g1");
    site
}

fn current_body(fake: &FakeS3) -> Vec<u8> {
    client(fake).get_object(BUCKET, "current").unwrap_or_default()
}

fn remote_body(fake: &FakeS3, key: &str) -> Vec<u8> {
    client(fake).get_object(BUCKET, key).unwrap_or_default()
}

fn remote_has(fake: &FakeS3, key: &str) -> bool {
    fake.state().iter().any(|object| object.key == key)
}

fn put_keys(fake: &FakeS3) -> Vec<String> {
    let prefix = format!("/{BUCKET}/");
    fake.requests()
        .into_iter()
        .filter(|request| request.method == "PUT")
        .filter_map(|request| {
            let key = request
                .path
                .strip_prefix(&prefix)
                .unwrap_or(&request.path)
                .to_string();
            (key != "current").then_some(key)
        })
        .collect()
}

fn mutation_count(fake: &FakeS3) -> usize {
    fake.requests()
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count()
}

fn publish_once(fake: &FakeS3, site: &Path, state: &Path, cache: &Path) {
    let report = transaction(fake, site, state, cache, options())
        .expect("open")
        .run()
        .expect("publish");
    assert_eq!(report.status, Status::Published, "{report:?}");
}

#[test]
fn a_full_publish_uploads_120_objects_and_the_manifest_last() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 120);
    let state = work.path().join("state");
    let cache = work.path().join("cache");

    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("publish");

    assert_eq!(report.status, Status::Published);
    assert_eq!(report.generation, "g1");
    assert_eq!(report.uploaded_objects, 122);
    assert!(report.uploaded_bytes > 0);
    assert_eq!(report.skipped_objects, 0);
    assert_eq!(report.multipart_objects, 0);
    assert_eq!(report.aliases.len(), 2);
    assert!(!report.dry_run);
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g1".to_string())
    );
    assert_eq!(current_body(&fake), b"g1");

    let puts = put_keys(&fake);
    assert_eq!(puts.len(), 122, "{puts:?}");
    assert_eq!(puts.last().unwrap(), "manifests/g1.json");
    assert!(fake.state().iter().all(|object| !object.key.contains("alias")));

    let conditions = fake.put_conditions();
    assert_eq!(conditions.len(), 1, "{conditions:?}");
    assert_eq!(conditions[0].if_match, None);
    assert_eq!(conditions[0].if_none_match.as_deref(), Some("*"));
    assert_eq!(conditions[0].body, "g1");
}

#[test]
fn an_unchanged_publish_uploads_and_promotes_nothing() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 120);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    let writes = mutation_count(&fake);
    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("second publish");

    assert_eq!(report.status, Status::Unchanged);
    assert_eq!(report.uploaded_objects, 0);
    assert_eq!(report.skipped_objects, 122);
    assert_eq!(mutation_count(&fake), writes);
    assert_eq!(fake.put_conditions().len(), 1);
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g1".to_string())
    );
}

#[test]
fn a_dry_run_plans_the_puts_and_writes_nothing() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 120);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    add_generation(&site, "g1b", &[PKG_A]);
    set_current(&site, "g1b");
    let writes = mutation_count(&fake);
    let report = transaction(
        &fake,
        &site,
        &state,
        &cache,
        TransactionOptions {
            dry_run: true,
            ..options()
        },
    )
    .expect("open")
    .run()
    .expect("dry run");

    assert!(report.dry_run);
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.uploaded_objects, 2);
    assert_eq!(report.multipart_objects, 0);
    assert_eq!(report.skipped_objects, 122);
    assert_eq!(mutation_count(&fake), writes);
    assert!(!site.join("manifests/g1b.json").exists());
    assert_eq!(current_body(&fake), b"g1");
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g1".to_string())
    );
}

#[test]
fn changed_immutable_content_conflicts_before_upload_and_repair_overwrites_it() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 120);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    fs::write(site.join("tarballs/pkg-7.tgz"), b"changed\n").unwrap();
    let writes = mutation_count(&fake);
    let error = transaction(&fake, &site, &state, &cache, options())
        .map(|_| ())
        .expect_err("conflict");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(error.to_string().contains("tarballs/pkg-7.tgz"), "{error}");
    assert_eq!(mutation_count(&fake), writes);
    assert_eq!(remote_body(&fake, "tarballs/pkg-7.tgz"), b"object 7\n");

    let repair = TransactionOptions {
        upload: UploadOptions {
            repair: true,
            ..UploadOptions::default()
        },
        ..options()
    };
    let report = transaction(&fake, &site, &state, &cache, repair)
        .expect("open")
        .run()
        .expect("repair");
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.uploaded_objects, 2);
    assert_eq!(report.skipped_objects, 120);
    assert_eq!(remote_body(&fake, "tarballs/pkg-7.tgz"), b"changed\n");
    assert_eq!(current_body(&fake), b"g1");
}

#[test]
fn a_second_generation_promotes_with_if_match_and_rolls_back() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 3);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    add_generation(&site, "g2", &[PKG_A]);
    set_current(&site, "g2");
    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("second generation");
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.uploaded_objects, 2);
    assert_eq!(current_body(&fake), b"g2");

    let expected_etag = format!("\"{}\"", md5_hex(b"g1"));
    let conditions = fake.put_conditions();
    assert_eq!(conditions.len(), 2, "{conditions:?}");
    assert_eq!(
        conditions[1].if_match.as_deref(),
        Some(expected_etag.as_str())
    );
    assert_eq!(conditions[1].if_none_match, None);
    assert_eq!(conditions[1].body, "g2");

    let rollback = transaction::rollback(
        credentials(&fake),
        Registry::Cargo,
        BUCKET,
        state.clone(),
        "g1",
        false,
        Duration::from_secs(5),
    )
    .expect("rollback");
    assert_eq!(rollback.status, Status::Published);
    assert_eq!(current_body(&fake), b"g1");
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g1".to_string())
    );
}

#[test]
fn a_later_generation_promotes_against_the_expected_recorded_base() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 3);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    fs::write(site.join("tarballs/pkg-later.tgz"), b"later\n").unwrap();
    add_generation(&site, "g4", &[PKG_A]);
    set_current(&site, "g4");
    let report = transaction(
        &fake,
        &site,
        &state,
        &cache,
        TransactionOptions {
            expected: Some("g1".to_string()),
            ..options()
        },
    )
    .expect("open")
    .run()
    .expect("later generation");
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.uploaded_objects, 3);
    assert_eq!(current_body(&fake), b"g4");
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g4".to_string())
    );
}

#[test]
fn a_stale_recorded_generation_is_refused_without_uploading() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 3);
    let state_a = work.path().join("state-a");
    let state_b = work.path().join("state-b");
    let cache_a = work.path().join("cache-a");
    let cache_b = work.path().join("cache-b");
    publish_once(&fake, &site, &state_a, &cache_a);

    add_generation(&site, "g5", &[PKG_A]);
    set_current(&site, "g5");
    publish_once(&fake, &site, &state_b, &cache_b);
    assert_eq!(current_body(&fake), b"g5");

    add_generation(&site, "g6", &[PKG_A]);
    set_current(&site, "g6");
    let writes = mutation_count(&fake);
    let error = transaction(&fake, &site, &state_a, &cache_a, options())
        .map(|_| ())
        .expect_err("stale conflict");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(
        error
            .to_string()
            .contains("remote current is 'g5', expected 'g1'"),
        "{error}"
    );
    assert_eq!(mutation_count(&fake), writes);
    assert_eq!(current_body(&fake), b"g5");
    assert_eq!(
        transaction::read_record(&state_a).unwrap(),
        Some("g1".to_string())
    );
}

#[test]
fn rollback_refuses_a_generation_without_a_remote_manifest() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 2);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);
    add_generation(&site, "g3", &[PKG_A]);

    let writes = mutation_count(&fake);
    let error = transaction::rollback(
        credentials(&fake),
        Registry::Cargo,
        BUCKET,
        state,
        "g3",
        false,
        Duration::from_secs(5),
    )
    .expect_err("missing manifest");
    assert_eq!(error.exit_code(), 4, "{error}");
    assert!(error.to_string().contains("has no manifests/g3.json"), "{error}");
    assert_eq!(mutation_count(&fake), writes);
    assert_eq!(current_body(&fake), b"g1");
}

#[test]
fn the_catalog_guard_refuses_dropping_a_live_version() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 2);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    add_generation(&site, "g7", &[PKG_B]);
    set_current(&site, "g7");
    let writes = mutation_count(&fake);
    let error = transaction(&fake, &site, &state, &cache, options())
        .map(|_| ())
        .expect_err("dropped version");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(
        error
            .to_string()
            .contains("drops 1 versions served by live generation g1"),
        "{error}"
    );
    assert!(error.to_string().contains("  a 1.0.0 (native)"), "{error}");
    assert_eq!(mutation_count(&fake), writes);
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g1".to_string())
    );

    let report = transaction(
        &fake,
        &site,
        &state,
        &cache,
        TransactionOptions {
            allow_drops: true,
            ..options()
        },
    )
    .expect("open")
    .run()
    .expect("allow drops");
    assert_eq!(report.status, Status::Published);
    assert_eq!(current_body(&fake), b"g7");

    add_generation(&site, "g8", &[PKG_A_YANKED, PKG_B]);
    set_current(&site, "g8");
    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("superset");
    assert_eq!(report.status, Status::Published);
    assert_eq!(current_body(&fake), b"g8");
}

#[test]
fn shared_blobs_upload_multipart_and_skip_when_unchanged() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let blobs = work.path().join("site/blobs/sha256");
    fs::create_dir_all(&blobs).unwrap();
    let hex = "b".repeat(64);
    fs::write(blobs.join(&hex), vec![0_u8; 300]).unwrap();
    let client = client(&fake);
    let upload = UploadOptions {
        single_put_max: 200,
        part_size: 128,
        repair: false,
    };

    let report =
        sync::sync_blobs(&client, &work.path().join("site"), "stuff-oci-blobs", &upload, false)
            .expect("blob sync");
    assert_eq!(report.uploaded_objects(), 1);
    assert_eq!(report.multipart.len(), 1);
    assert_eq!(report.skipped_objects(), 0);
    let object = fake
        .state()
        .into_iter()
        .find(|object| object.key == format!("sha256/{hex}"))
        .expect("uploaded blob");
    assert_eq!(object.bucket, "stuff-oci-blobs");
    assert_eq!(object.size, 300);

    let rerun =
        sync::sync_blobs(&client, &work.path().join("site"), "stuff-oci-blobs", &upload, false)
            .expect("second blob sync");
    assert_eq!(rerun.uploaded_objects(), 0);
    assert_eq!(rerun.skipped_objects(), 1);
}

#[test]
fn an_interrupted_upload_resumes_without_rewriting_uploaded_objects() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 5);
    let state = work.path().join("state");
    let cache = work.path().join("cache");

    let pending = transaction(&fake, &site, &state, &cache, options()).expect("open");
    fake.inject_failures(1, 500, "0");
    let error = pending.run().expect_err("transport failure");
    assert_eq!(error.exit_code(), 5, "{error}");
    assert!(!remote_has(&fake, "manifests/g1.json"));
    assert!(!remote_has(&fake, "current"));

    for index in 0..3 {
        let key = format!("tarballs/pkg-{index}.tgz");
        let body = fs::read(site.join(&key)).unwrap();
        fake.seed(BUCKET, &key, &body, "application/octet-stream");
    }
    let present = fake.state().len();
    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("resume");
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.skipped_objects, present);
    assert_eq!(report.uploaded_objects + report.skipped_objects, 7);
    assert!(remote_has(&fake, "manifests/g1.json"));
    assert_eq!(current_body(&fake), b"g1");
}

#[test]
fn an_interrupted_promotion_is_safe_to_rerun() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 2);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    publish_once(&fake, &site, &state, &cache);

    add_generation(&site, "g2", &[PKG_A]);
    set_current(&site, "g2");
    let client = client(&fake);
    let uploaded =
        sync::sync(&client, &site, BUCKET, &UploadOptions::default(), &[], false)
            .expect("upload without promotion");
    assert_eq!(uploaded.uploaded_objects(), 2);
    assert_eq!(current_body(&fake), b"g1");
    let conditions = fake.put_conditions().len();

    let pending = transaction(&fake, &site, &state, &cache, options()).expect("open");
    fake.inject_failures(1, 500, "0");
    let error = pending.run().expect_err("interrupted promotion");
    assert_eq!(error.exit_code(), 5, "{error}");
    assert_eq!(current_body(&fake), b"g1");
    assert_eq!(fake.put_conditions().len(), conditions);

    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("retry promotion");
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.uploaded_objects, 0);
    assert_eq!(current_body(&fake), b"g2");
}

#[test]
fn a_held_file_lock_blocks_the_publish_transaction() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 2);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    fs::create_dir_all(&state).unwrap();
    let lock_path = state.join(transaction::LOCK_FILE);
    let holder = FileLock::acquire_blocking(&lock_path).expect("hold the lock");
    let writes = mutation_count(&fake);

    let credentials = credentials(&fake);
    let state_path = state.clone();
    let cache_path = cache.clone();
    let handle = std::thread::spawn(move || {
        Transaction::begin(
            credentials,
            Registry::Cargo,
            &Config::default(),
            state_path,
            cache_path,
            TransactionOptions {
                lock_wait: Duration::from_millis(200),
                ..TransactionOptions::default()
            },
        )
        .map(|_| ())
    });
    let error = handle.join().expect("thread").expect_err("lock timeout");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert_eq!(mutation_count(&fake), writes);
    assert!(!remote_has(&fake, "current"));

    drop(holder);
    assert!(FileLock::try_acquire(&lock_path).unwrap().is_some());
    let report = transaction(&fake, &site, &state, &cache, options())
        .expect("open")
        .run()
        .expect("publish after release");
    assert_eq!(report.status, Status::Published);
    assert_eq!(current_body(&fake), b"g1");
}

#[test]
fn a_second_begin_conflicts_while_the_first_is_still_staging() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let site = base_site(work.path(), 2);
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    let lock_wait = Duration::from_millis(200);

    let first = Transaction::begin(
        credentials(&fake),
        Registry::Cargo,
        &Config::default(),
        state.clone(),
        cache.clone(),
        TransactionOptions {
            lock_wait,
            ..options()
        },
    )
    .expect("first begin");
    let writes = mutation_count(&fake);

    let error = Transaction::begin(
        credentials(&fake),
        Registry::Cargo,
        &Config::default(),
        state.clone(),
        cache,
        TransactionOptions {
            lock_wait,
            ..options()
        },
    )
    .map(|_| ())
    .expect_err("second begin");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(error.to_string().contains("another publish holds"), "{error}");
    assert_eq!(mutation_count(&fake), writes);
    assert!(!remote_has(&fake, "current"));

    let report = first
        .stage_complete(site)
        .expect("stage complete")
        .run()
        .expect("first publisher still wins");
    assert_eq!(report.status, Status::Published);
    assert_eq!(current_body(&fake), b"g1");
}

#[test]
fn a_first_publication_with_no_remote_or_local_current_succeeds() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let state = work.path().join("state");
    let cache = work.path().join("cache");
    let site = work.path().join("site");
    assert!(!state.exists());
    assert!(!site.exists());
    assert!(fake.state().is_empty());

    let pending = Transaction::begin(
        credentials(&fake),
        Registry::Cargo,
        &Config::default(),
        state.clone(),
        cache,
        options(),
    )
    .expect("begin without a local site");
    let site = base_site(work.path(), 2);
    let report = pending
        .stage_complete(site)
        .expect("stage complete")
        .run()
        .expect("first publish");
    assert_eq!(report.status, Status::Published);
    assert_eq!(report.generation, "g1");
    assert_eq!(current_body(&fake), b"g1");
    assert_eq!(
        transaction::read_record(&state).unwrap(),
        Some("g1".to_string())
    );
}

fn seed_generation(fake: &FakeS3, generation: &str, body: &[u8]) {
    fake.seed(BUCKET, "blobs/a", body, "application/octet-stream");
    let manifest = format!(
        r#"{{"schemaVersion":1,"objects":[{{"key":"blobs/a","size":{},"etag":"{}"}}]}}"#,
        body.len(),
        md5_hex(body)
    );
    fake.seed(
        BUCKET,
        &format!("manifests/{generation}.json"),
        manifest.as_bytes(),
        "application/json",
    );
}

#[test]
fn promote_replaces_current_with_if_match_on_the_observed_etag() {
    let fake = FakeS3::start_verified();
    seed_generation(&fake, "g1", b"artifact");
    fake.seed(BUCKET, "current", b"g0", "text/plain");

    let promotion = promote::promote(&client(&fake), BUCKET, "g1", None).expect("promote");
    assert!(promotion.promoted);
    assert_eq!(current_body(&fake), b"g1");
    let expected = format!("\"{}\"", md5_hex(b"g0"));
    let conditions = fake.put_conditions();
    assert_eq!(conditions.len(), 1, "{conditions:?}");
    assert_eq!(
        conditions[0].if_match.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(conditions[0].if_none_match, None);
    assert_eq!(conditions[0].body, "g1");
}

#[test]
fn a_competing_writer_gets_412_and_current_is_not_overwritten() {
    let fake = FakeS3::start_verified();
    seed_generation(&fake, "g1", b"artifact");
    fake.seed(BUCKET, "current", b"g0", "text/plain");
    fake.change_current_on_head(true);

    let error = promote::promote(&client(&fake), BUCKET, "g1", None)
        .expect_err("412");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(
        error.to_string().contains("current changed during promotion"),
        "{error}"
    );
    assert_eq!(current_body(&fake), b"g2");
}

#[test]
fn a_missing_generation_object_fails_before_current_is_touched() {
    let fake = FakeS3::start_verified();
    let manifest = format!(
        r#"{{"schemaVersion":1,"objects":[{{"key":"blobs/missing","size":8,"etag":"{}"}}]}}"#,
        md5_hex(b"artifact")
    );
    fake.seed(BUCKET, "manifests/g1.json", manifest.as_bytes(), "application/json");
    fake.seed(BUCKET, "current", b"g0", "text/plain");

    let error = promote::promote(&client(&fake), BUCKET, "g1", None)
        .expect_err("missing object");
    assert_eq!(error.exit_code(), 4, "{error}");
    assert!(
        error.to_string().contains("generation is incomplete: missing blobs/missing"),
        "{error}"
    );
    assert_eq!(current_body(&fake), b"g0");
    assert!(fake.put_conditions().is_empty());
}

#[test]
fn an_expected_generation_mismatch_does_not_write() {
    let fake = FakeS3::start_verified();
    seed_generation(&fake, "g1", b"artifact");
    fake.seed(BUCKET, "current", b"g0", "text/plain");

    let error = promote::promote(&client(&fake), BUCKET, "g1", Some("other"))
        .expect_err("expected mismatch");
    assert_eq!(error.exit_code(), 3, "{error}");
    assert_eq!(current_body(&fake), b"g0");
    assert!(fake.put_conditions().is_empty());
}

#[test]
fn the_first_publication_uses_if_none_match_star() {
    let fake = FakeS3::start_verified();
    seed_generation(&fake, "g1", b"artifact");

    let promotion = promote::promote(&client(&fake), BUCKET, "g1", None).expect("bootstrap");
    assert!(promotion.promoted);
    assert_eq!(current_body(&fake), b"g1");
    let conditions = fake.put_conditions();
    assert_eq!(conditions.len(), 1, "{conditions:?}");
    assert_eq!(conditions[0].if_match, None);
    assert_eq!(conditions[0].if_none_match.as_deref(), Some("*"));
    assert_eq!(conditions[0].body, "g1");
}

#[test]
fn promoting_the_current_generation_verifies_without_writing() {
    let fake = FakeS3::start_verified();
    seed_generation(&fake, "g1", b"artifact");
    fake.seed(BUCKET, "current", b"g1", "text/plain");

    let promotion = promote::promote(&client(&fake), BUCKET, "g1", None).expect("verify");
    assert!(!promotion.promoted);
    assert!(fake.put_conditions().is_empty());
}
