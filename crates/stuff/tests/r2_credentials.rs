#![forbid(unsafe_code)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use stuff::publish::r2::credentials::resolve;
use stuff::publish::{Environment, ExitCode};

use support::write_executable;

const FAKE_CF: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth whoami")
    if [ -n "$FAKE_CF_LOGIN_FAIL" ]; then
      echo "Not logged in. Run cf auth login." >&2
      exit 1
    fi
    printf '%s\n' '{"accounts":[{"id":"acct"}]}'
    ;;
  "accounts tokens")
    if [ -n "$FAKE_CF_NO_TOKENS" ]; then
      printf '%s\n' '[]'
    else
      printf '%s\n' '[{"id":"other","status":"active","policies":[]},{"id":"r2","status":"active","policies":[{"permission_groups":[{"name":"Workers R2 Storage Write"}]}]}]'
    fi
    ;;
  "r2 temporary-credentials")
    parent=""
    while [ $# -gt 0 ]; do
      if [ "$1" = "--parent-access-key-id" ]; then parent=$2; fi
      shift
    done
    printf '%s\n' "$parent" >> "$FAKE_CF_LOG"
    if [ -n "$FAKE_CF_MINT_FAIL" ]; then
      echo "401 token expired" >&2
      exit 1
    fi
    if [ -n "$FAKE_CF_ALLOW" ] || [ "$parent" = "r2" ]; then
      printf '%s\n' '{"accessKeyId":"access","secretAccessKey":"secret","sessionToken":"session"}'
    else
      echo "denied" >&2
      exit 1
    fi
    ;;
  *)
    echo "unexpected cf invocation: $*" >&2
    exit 1
    ;;
esac
"#;

struct FakeCf {
    _work: tempfile::TempDir,
    log: PathBuf,
    bin: PathBuf,
}

fn fake_cf() -> FakeCf {
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("cf");
    write_executable(&script, FAKE_CF);
    let log = work.path().join("cf.log");
    FakeCf { _work: work, log, bin }
}

impl FakeCf {
    fn environment(&self, extra: &[(&str, &str)]) -> Environment {
        let mut pairs = vec![
            ("PATH".to_string(), self.bin.to_str().unwrap().to_string()),
            ("FAKE_CF_LOG".to_string(), self.log.to_str().unwrap().to_string()),
        ];
        pairs.extend(
            extra.iter().map(|(name, value)| ((*name).to_string(), (*value).to_string())),
        );
        Environment::from_pairs(pairs)
    }

    fn parents(&self) -> Vec<String> {
        read_log(&self.log)
    }
}

fn read_log(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(text) => text.split_whitespace().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn mints_scoped_credentials_from_the_r2_parent() {
    let fake = fake_cf();
    let credentials = resolve(&fake.environment(&[]), "bucket").unwrap();
    assert_eq!(credentials.endpoint, "https://acct.r2.cloudflarestorage.com");
    assert_eq!(credentials.access_key_id, "access");
    assert_eq!(credentials.secret_access_key, "secret");
    assert_eq!(credentials.session_token.as_deref(), Some("session"));
    assert_eq!(credentials.region, "auto");
    assert_eq!(fake.parents(), vec!["r2"]);
}

#[test]
fn parent_override_skips_token_discovery() {
    let fake = fake_cf();
    let env = fake.environment(&[
        ("R2_PARENT_ACCESS_KEY_ID", "other"),
        ("FAKE_CF_ALLOW", "1"),
        ("FAKE_CF_NO_TOKENS", "1"),
    ]);
    let credentials = resolve(&env, "bucket").unwrap();
    assert_eq!(credentials.access_key_id, "access");
    assert_eq!(fake.parents(), vec!["other"]);
}

#[test]
fn no_active_token_fails_without_printing_credentials() {
    let fake = fake_cf();
    let error = resolve(&fake.environment(&[("FAKE_CF_NO_TOKENS", "1")]), "bucket").unwrap_err();
    assert_eq!(error.classification(), ExitCode::Usage);
    let message = error.to_string();
    assert!(message.contains("no active account token"), "{message}");
    assert!(!message.contains("secret"), "{message}");
    assert!(fake.parents().is_empty(), "{:?}", fake.parents());
}

#[test]
fn ttl_must_stay_within_the_api_limit() {
    let fake = fake_cf();
    let error = resolve(
        &fake.environment(&[("R2_TEMP_CREDENTIALS_TTL", "604801")]),
        "bucket",
    )
    .unwrap_err();
    assert_eq!(error.classification(), ExitCode::Usage);
    assert!(error.to_string().contains("R2_TEMP_CREDENTIALS_TTL"), "{error}");
}

#[test]
fn direct_r2_s3_environment_wins_over_cf() {
    let fake = fake_cf();
    let env = fake.environment(&[
        ("R2_S3_ENDPOINT", "https://account.r2.cloudflarestorage.com/"),
        ("R2_S3_ACCESS_KEY_ID", "direct-access"),
        ("R2_S3_SECRET_ACCESS_KEY", "direct-secret"),
        ("R2_S3_SESSION_TOKEN", "direct-session"),
    ]);
    let credentials = resolve(&env, "bucket").unwrap();
    assert_eq!(credentials.endpoint, "https://account.r2.cloudflarestorage.com");
    assert_eq!(credentials.access_key_id, "direct-access");
    assert_eq!(credentials.secret_access_key, "direct-secret");
    assert_eq!(credentials.session_token.as_deref(), Some("direct-session"));
    assert!(!fake.log.exists(), "cf was invoked despite direct credentials");
}

#[test]
fn missing_cf_login_is_an_actionable_config_error() {
    let fake = fake_cf();
    let error = resolve(&fake.environment(&[("FAKE_CF_LOGIN_FAIL", "1")]), "bucket").unwrap_err();
    assert_eq!(error.classification(), ExitCode::Usage);
    let message = error.to_string();
    assert!(message.contains("Not logged in"), "{message}");
    assert!(message.contains("cf auth login"), "{message}");
}

#[test]
fn expired_token_is_a_config_error_and_tries_every_parent() {
    let fake = fake_cf();
    let error = resolve(&fake.environment(&[("FAKE_CF_MINT_FAIL", "1")]), "bucket").unwrap_err();
    assert_eq!(error.classification(), ExitCode::Usage);
    let message = error.to_string();
    assert!(message.contains("token expired"), "{message}");
    assert!(!message.contains("secret"), "{message}");
    assert_eq!(fake.parents(), vec!["r2", "other"]);
}
