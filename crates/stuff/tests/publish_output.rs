#![forbid(unsafe_code)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use deser_value::Value;
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

const BINARY: &str = env!("CARGO_BIN_EXE_stuff");
const LAYOUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/oci/layout");
const REPOSITORY: &str = "example/app";
const OTHER_REPOSITORY: &str = "example/other";

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch_root() -> tempfile::TempDir {
    let base = repo_root().join(".work/tmp");
    std::fs::create_dir_all(&base).expect("create .work/tmp");
    tempfile::Builder::new()
        .prefix("stuff-publish-output.")
        .tempdir_in(&base)
        .expect("scratch directory under .work/tmp")
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

fn oci_args(repository: &str, json: bool) -> Vec<&str> {
    let mut args = vec![
        "publish",
        "oci",
        "--layout",
        LAYOUT,
        "--repository",
        repository,
    ];
    if json {
        args.push("--json");
    }
    args
}

fn single_json_line(run: &Run) -> Value {
    assert_eq!(
        run.stdout.lines().count(),
        1,
        "expected exactly one JSON line, got {:?}",
        run.stdout
    );
    deser_json::from_str(run.stdout.trim()).expect("stdout is one JSON object")
}

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value
        .as_map()
        .and_then(|map| map.get(name))
        .unwrap_or_else(|| panic!("missing {name} in {value:?}"))
}

fn text_field(value: &Value, name: &str) -> String {
    field(value, name)
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string in {value:?}"))
        .to_string()
}

fn number_field(value: &Value, name: &str) -> u64 {
    field(value, name)
        .as_u64()
        .unwrap_or_else(|| panic!("{name} is not a number in {value:?}"))
}

fn bool_field(value: &Value, name: &str) -> bool {
    field(value, name)
        .as_bool()
        .unwrap_or_else(|| panic!("{name} is not a boolean in {value:?}"))
}

fn record_path(work: &Path) -> PathBuf {
    work.join("state/oci/transaction/promoted-generation")
}

#[test]
fn json_reports_published_unchanged_and_conflict() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();

    let first = run(work.path(), &fake, &oci_args(REPOSITORY, true));
    assert_eq!(first.code, 0, "stdout={} stderr={}", first.stdout, first.stderr);
    assert_eq!(first.stderr, "");
    let published = single_json_line(&first);
    println!("published exit={} stdout={}", first.code, first.stdout.trim());
    assert_eq!(text_field(&published, "registry"), "oci");
    assert_eq!(text_field(&published, "status"), "published");
    assert_eq!(text_field(&published, "generation").len(), 24);
    assert!(!bool_field(&published, "dry_run"));
    assert!(number_field(&published, "uploaded_objects") > 0, "{published:?}");
    assert!(number_field(&published, "uploaded_bytes") > 0, "{published:?}");
    let generation = text_field(&published, "generation");

    let second = run(work.path(), &fake, &oci_args(REPOSITORY, true));
    assert_eq!(second.code, 0, "stdout={} stderr={}", second.stdout, second.stderr);
    assert_eq!(second.stderr, "");
    let unchanged = single_json_line(&second);
    println!("unchanged exit={} stdout={}", second.code, second.stdout.trim());
    assert_eq!(text_field(&unchanged, "registry"), "oci");
    assert_eq!(text_field(&unchanged, "status"), "unchanged");
    assert_eq!(text_field(&unchanged, "generation"), generation);
    assert_eq!(number_field(&unchanged, "uploaded_objects"), 0);

    let record = record_path(work.path());
    assert!(record.is_file(), "the first publish wrote no {}", record.display());
    std::fs::write(&record, "stale-expected\n").expect("write a stale expected generation");

    let conflict = run(work.path(), &fake, &oci_args(OTHER_REPOSITORY, true));
    assert_eq!(conflict.code, 3, "stdout={} stderr={}", conflict.stdout, conflict.stderr);
    assert_eq!(conflict.stderr, "");
    let conflicted = single_json_line(&conflict);
    println!("conflict exit={} stdout={}", conflict.code, conflict.stdout.trim());
    assert_eq!(text_field(&conflicted, "registry"), "oci");
    assert_eq!(text_field(&conflicted, "status"), "conflict");
    assert!(field(&conflicted, "generation").as_str().is_none(), "{conflicted:?}");
    assert!(!bool_field(&conflicted, "dry_run"));
    assert_eq!(number_field(&conflicted, "uploaded_objects"), 0);
    assert_eq!(number_field(&conflicted, "uploaded_bytes"), 0);
    assert_eq!(number_field(&conflicted, "skipped_objects"), 0);
}

#[test]
fn human_output_is_one_stable_line_per_outcome() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();

    let published = run(work.path(), &fake, &oci_args(REPOSITORY, false));
    assert_eq!(published.code, 0, "stdout={} stderr={}", published.stdout, published.stderr);
    assert_eq!(published.stderr, "");
    assert_eq!(published.stdout.lines().count(), 1, "{:?}", published.stdout);
    println!("human published exit={} stdout={}", published.code, published.stdout.trim());
    assert!(
        published.stdout.starts_with("oci published generation "),
        "{:?}",
        published.stdout
    );
    assert!(published.stdout.contains(" uploaded "), "{:?}", published.stdout);
    assert!(published.stdout.contains(" bytes), skipped "), "{:?}", published.stdout);

    let unchanged = run(work.path(), &fake, &oci_args(REPOSITORY, false));
    assert_eq!(unchanged.code, 0, "stdout={} stderr={}", unchanged.stdout, unchanged.stderr);
    assert_eq!(unchanged.stderr, "");
    assert_eq!(unchanged.stdout.lines().count(), 1, "{:?}", unchanged.stdout);
    println!("human unchanged exit={} stdout={}", unchanged.code, unchanged.stdout.trim());
    assert!(
        unchanged.stdout.starts_with("oci unchanged generation "),
        "{:?}",
        unchanged.stdout
    );
}

#[test]
fn usage_and_config_failures_exit_two_with_nothing_on_stdout() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let work_path = work.path().to_path_buf();

    let usage = run(&work_path, &fake, &["publish", "cargo"]);
    println!("usage exit={} stdout={:?} stderr={}", usage.code, usage.stdout, usage.stderr.trim());
    assert_eq!(usage.code, 2, "stdout={} stderr={}", usage.stdout, usage.stderr);
    assert_eq!(usage.stdout, "");
    assert!(usage.stderr.starts_with("stuff: usage: "), "{:?}", usage.stderr);

    std::fs::create_dir_all(work_path.join("config")).expect("create config root");
    std::fs::write(work_path.join("config/config.json"), "{not json")
        .expect("write a broken config");
    let config = run(&work_path, &fake, &oci_args(REPOSITORY, true));
    println!("config exit={} stdout={:?} stderr={}", config.code, config.stdout, config.stderr.trim());
    assert_eq!(config.code, 2, "stdout={} stderr={}", config.stdout, config.stderr);
    assert_eq!(config.stdout, "");
    assert!(config.stderr.starts_with("stuff: config: "), "{:?}", config.stderr);
}

#[test]
fn integrity_failures_exit_four_with_nothing_on_stdout() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    let result = run(
        work.path(),
        &fake,
        &["rollback", "cargo", "g9", "--json"],
    );
    println!("integrity exit={} stdout={:?} stderr={}", result.code, result.stdout, result.stderr.trim());
    assert_eq!(result.code, 4, "stdout={} stderr={}", result.stdout, result.stderr);
    assert_eq!(result.stdout, "");
    assert!(result.stderr.starts_with("stuff: integrity: "), "{:?}", result.stderr);
}

#[test]
fn transport_failures_exit_five_with_nothing_on_stdout() {
    let work = scratch_root();
    let fake = FakeS3::start_verified();
    fake.inject_failures(6, 429, "0");
    let result = run(work.path(), &fake, &oci_args(REPOSITORY, true));
    println!("transport exit={} stdout={:?} stderr={}", result.code, result.stdout, result.stderr.trim());
    assert_eq!(result.code, 5, "stdout={} stderr={}", result.stdout, result.stderr);
    assert_eq!(result.stdout, "");
    assert!(result.stderr.starts_with("stuff: transport: "), "{:?}", result.stderr);
}
