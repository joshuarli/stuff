#![forbid(unsafe_code)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use stuff::publish::r2::pull::md5_hex;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const BINARY: &str = env!("CARGO_BIN_EXE_stuff");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn environment(work: &Path, fake: &FakeS3) -> Vec<(String, String)> {
    [
        ("R2_S3_ENDPOINT", fake.base_url().to_string()),
        ("R2_S3_ACCESS_KEY_ID", ACCESS_KEY.to_string()),
        ("R2_S3_SECRET_ACCESS_KEY", SECRET_KEY.to_string()),
        ("STUFF_CONFIG_HOME", work.join("config").display().to_string()),
        ("STUFF_CACHE_HOME", work.join("cache").display().to_string()),
        ("STUFF_STATE_HOME", work.join("state").display().to_string()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

fn run(work: &Path, fake: &FakeS3, args: &[&str]) -> Run {
    let output = Command::new(BINARY)
        .args(args)
        .env_clear()
        .envs(environment(work, fake))
        .output()
        .expect("run the stuff binary");
    Run {
        code: output.status.code().expect("the binary was killed"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

fn seed_generation(fake: &FakeS3, bucket: &str, generation: &str) {
    let key = format!("generations/{generation}/catalog.json");
    let body = format!("{{\"generation\":\"{generation}\"}}").into_bytes();
    fake.seed(bucket, &key, &body, "application/json");
    let manifest = format!(
        "{{\"schemaVersion\":1,\"objects\":[{{\"key\":\"{key}\",\"size\":{},\"etag\":\"{}\"}}]}}",
        body.len(),
        md5_hex(&body)
    );
    fake.seed(
        bucket,
        &format!("manifests/{generation}.json"),
        manifest.as_bytes(),
        "application/json",
    );
}

fn current_body(fake: &FakeS3, bucket: &str) -> Option<Vec<u8>> {
    fake.state()
        .into_iter()
        .find(|object| object.bucket == bucket && object.key == "current")
        .map(|object| object.body)
}

fn mutations(fake: &FakeS3) -> usize {
    fake.requests()
        .iter()
        .filter(|request| matches!(request.method.as_str(), "PUT" | "POST" | "DELETE"))
        .count()
}

fn record(work: &Path, registry: &str) -> String {
    let state = work.join("state").join(registry);
    let state = if registry == "oci" { state.join("transaction") } else { state };
    std::fs::read_to_string(state.join("promoted-generation")).unwrap()
}

#[test]
fn rollback_promotes_the_target_generation_for_every_registry() {
    for (registry, bucket, blobs) in [
        ("cargo", "stuff-mycrates", None),
        ("npm", "stuff-mynpm", None),
        ("pypi", "stuff-mypypi", None),
        ("oci", "stuff-oci", Some("stuff-oci-blobs")),
    ] {
        let fake = FakeS3::start_verified();
        let work = tempfile::tempdir().unwrap();
        seed_generation(&fake, bucket, "g1");
        seed_generation(&fake, bucket, "g2");
        fake.seed(bucket, "current", b"g2", "text/plain");
        if let Some(blobs) = blobs {
            fake.seed(blobs, "sha256/abc", b"blob", "application/octet-stream");
        }

        let result = run(work.path(), &fake, &["rollback", registry, "g1"]);
        assert_eq!(result.code, 0, "{registry}: {}", result.stderr);
        assert_eq!(
            current_body(&fake, bucket).as_deref(),
            Some(b"g1".as_slice()),
            "{registry} did not promote the target generation"
        );
        assert_eq!(record(work.path(), registry), "g1\n", "{registry} record");

        let conditions = fake.put_conditions();
        assert_eq!(conditions.len(), 1, "{registry}: {conditions:?}");
        assert!(conditions[0].if_match.is_some(), "{registry} used an unconditional put");

        if let Some(blobs) = blobs {
            let prefix = format!("/{blobs}");
            assert!(
                fake.requests().iter().all(|request| !request.path.starts_with(&prefix)),
                "{registry} touched the blobs bucket"
            );
        }
    }
}

#[test]
fn a_competing_promotion_exits_three() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    seed_generation(&fake, "stuff-mycrates", "g1");
    seed_generation(&fake, "stuff-mycrates", "g2");
    fake.seed("stuff-mycrates", "current", b"g2", "text/plain");
    fake.change_current_on_head(true);

    let result = run(work.path(), &fake, &["rollback", "cargo", "g1"]);
    assert_eq!(result.code, 3, "{}", result.stderr);
    assert!(result.stderr.contains("conflict"), "{}", result.stderr);
    assert!(result.stderr.contains("current changed during promotion"), "{}", result.stderr);
    assert_eq!(
        current_body(&fake, "stuff-mycrates").as_deref(),
        Some(b"g2".as_slice())
    );
}

#[test]
fn a_missing_or_corrupt_manifest_exits_four() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    seed_generation(&fake, "stuff-mycrates", "g1");
    fake.seed("stuff-mycrates", "current", b"g1", "text/plain");

    let missing = run(work.path(), &fake, &["rollback", "cargo", "g9"]);
    assert_eq!(missing.code, 4, "{}", missing.stderr);
    assert!(missing.stderr.contains("has no manifests/g9.json"), "{}", missing.stderr);
    assert_eq!(
        current_body(&fake, "stuff-mycrates").as_deref(),
        Some(b"g1".as_slice())
    );

    fake.seed(
        "stuff-mycrates",
        "manifests/g8.json",
        b"{not json",
        "application/json",
    );
    let corrupt = run(work.path(), &fake, &["rollback", "cargo", "g8"]);
    assert_eq!(corrupt.code, 4, "{}", corrupt.stderr);
    assert!(corrupt.stderr.contains("integrity"), "{}", corrupt.stderr);
    assert_eq!(
        current_body(&fake, "stuff-mycrates").as_deref(),
        Some(b"g1".as_slice())
    );
}

fn local_catalog(work: &Path) -> PathBuf {
    let directory = work.join("catalog");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("catalog.json"),
        "{\"schema_version\":1,\"registry\":\"cargo\",\"generation\":\"g1\",\"packages\":[]}\n",
    )
    .unwrap();
    directory
}

#[test]
fn doctor_verifies_the_live_generation_without_writing() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let catalog = local_catalog(work.path());
    seed_generation(&fake, "stuff-mycrates", "g1");
    fake.seed("stuff-mycrates", "current", b"g1", "text/plain");

    let before = mutations(&fake);
    let result = run(
        work.path(),
        &fake,
        &["doctor", "--registry", "cargo", "--cargo-dir", catalog.to_str().unwrap()],
    );
    assert_eq!(
        result.code, 0,
        "stdout={}\nstderr={}",
        result.stdout, result.stderr
    );
    assert!(
        result.stdout.contains("ok: cargo live generation"),
        "{}",
        result.stdout
    );
    assert_eq!(mutations(&fake), before, "doctor wrote to the bucket");
}

#[test]
fn doctor_fails_the_live_generation_check_without_writing() {
    let fake = FakeS3::start_verified();
    let work = tempfile::tempdir().unwrap();
    let catalog = local_catalog(work.path());
    fake.seed("stuff-mycrates", "current", b"g1", "text/plain");

    let before = mutations(&fake);
    let result = run(
        work.path(),
        &fake,
        &["doctor", "--registry", "cargo", "--cargo-dir", catalog.to_str().unwrap()],
    );
    assert_eq!(result.code, 1, "stdout={}\nstderr={}", result.stdout, result.stderr);
    assert!(
        result.stdout.contains("FAIL: cargo live generation"),
        "{}",
        result.stdout
    );
    assert!(result.stdout.contains("[exit 4]"), "{}", result.stdout);
    assert_eq!(mutations(&fake), before, "doctor wrote to the bucket");
}
