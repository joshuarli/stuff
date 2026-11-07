use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use deser_value::Value;

use crate::publish::{Environment, PublishError};

use super::client::Credentials;

const DEFAULT_TTL_SECONDS: i64 = 3600;
const MAX_TTL_SECONDS: i64 = 604_800;
const ATTEMPTS: u32 = 3;
pub fn resolve(
    env: &Environment,
    profile: &str,
    bucket: &str,
) -> Result<Credentials, PublishError> {
    if let Some(credentials) = direct_credentials(env) {
        return Ok(credentials);
    }
    if bucket.is_empty() {
        return Err(config("bucket is empty"));
    }
    let cf = Cf { env, profile };
    let account = account_id(&cf)?;
    let parents = parent_keys(&cf)?;
    let mut last = None;
    for parent in &parents {
        match mint(&cf, bucket, parent, &account) {
            Ok(credentials) => return Ok(credentials),
            Err(MintFailure::Rejected(detail)) => last = Some(detail),
            Err(MintFailure::Fatal(error)) => return Err(error),
        }
    }
    Err(config(
        last.unwrap_or_else(|| "no active account token can mint R2 credentials".to_string()),
    ))
}

fn direct_credentials(env: &Environment) -> Option<Credentials> {
    let configured = ["R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY"]
        .iter()
        .all(|name| env.non_empty(name).is_some());
    if !configured {
        return None;
    }
    Credentials::from_env(|name| env.get(name).map(str::to_string)).ok()
}

struct Cf<'a> {
    env: &'a Environment,
    profile: &'a str,
}

impl Cf<'_> {
    fn login_hint(&self) -> String {
        format!(
            "run `cf auth create {}` to log in to the publishing profile, or export R2_S3_ENDPOINT, R2_S3_ACCESS_KEY_ID and R2_S3_SECRET_ACCESS_KEY",
            self.profile
        )
    }
}

fn account_id(cf: &Cf) -> Result<String, PublishError> {
    if let Some(configured) = cf.env.non_empty("R2_ACCOUNT_ID") {
        return Ok(configured.to_string());
    }
    let output = run_cf(cf, &["auth", "whoami"]).map_err(|detail| {
        config(format!(
            "cannot determine the R2 account with cf profile {}: {detail} ({})",
            cf.profile,
            cf.login_hint()
        ))
    })?;
    let document: Value = deser_json::from_str(&output).map_err(|error| {
        config(format!("cf auth whoami returned invalid JSON: {error}"))
    })?;
    let account = document
        .as_map()
        .and_then(|map| map.get("accounts"))
        .and_then(|value| value.as_seq())
        .and_then(|accounts| accounts.iter().next())
        .and_then(|value| value.as_map())
        .and_then(|account| account.get("id"))
        .and_then(|value| value.as_str())
        .filter(|id| !id.is_empty());
    match account {
        Some(id) => Ok(id.to_string()),
        None => Err(config(format!(
            "cf auth whoami returned no account for profile {} ({})",
            cf.profile,
            cf.login_hint()
        ))),
    }
}

fn parent_keys(cf: &Cf) -> Result<Vec<String>, PublishError> {
    if let Some(configured) = cf.env.non_empty("R2_PARENT_ACCESS_KEY_ID") {
        return Ok(vec![configured.to_string()]);
    }
    let output = run_cf(cf, &["accounts", "tokens", "list", "--per-page", "50"])
        .map_err(|detail| config(format!("cannot list account tokens: {detail}")))?;
    let document: Value = deser_json::from_str(&output).map_err(|error| {
        config(format!("cf accounts tokens list returned invalid JSON: {error}"))
    })?;
    let tokens: Vec<&Value> = match document.as_map() {
        Some(map) => match map.get("result") {
            None => Vec::new(),
            Some(result) => result
                .as_seq()
                .ok_or_else(|| {
                    config("cf accounts tokens list returned a result that is not an array")
                })?
                .iter()
                .collect(),
        },
        None => document
            .as_seq()
            .ok_or_else(|| config("cf accounts tokens list returned unexpected JSON"))?
            .iter()
            .collect(),
    };
    let mut active: Vec<(bool, String)> = Vec::new();
    for token in tokens {
        let token = token.as_map().ok_or_else(|| {
            config("cf accounts tokens list returned an entry that is not an object")
        })?;
        if token.get("status").and_then(|value| value.as_str()) != Some("active") {
            continue;
        }
        let Some(id) = token.get("id").and_then(|value| value.as_str()).filter(|id| !id.is_empty()) else {
            continue;
        };
        let mentions_r2 = token.get("policies").map(mentions_r2).unwrap_or(false);
        active.push((mentions_r2, id.to_string()));
    }
    active.sort_by_key(|(mentions_r2, _)| std::cmp::Reverse(*mentions_r2));
    Ok(active.into_iter().map(|(_, id)| id).collect())
}

fn mentions_r2(policies: &Value) -> bool {
    deser_json::to_string(policies)
        .map(|text| text.to_lowercase().contains("r2"))
        .unwrap_or(false)
}

fn ttl_seconds(env: &Environment) -> Result<i64, PublishError> {
    let Some(text) = env.get("R2_TEMP_CREDENTIALS_TTL") else {
        return Ok(DEFAULT_TTL_SECONDS);
    };
    let value = text
        .trim()
        .parse::<i64>()
        .map_err(|_| {
            config(format!(
                "R2_TEMP_CREDENTIALS_TTL must be an integer number of seconds, got {text:?}"
            ))
        })?;
    if value <= 0 || value > MAX_TTL_SECONDS {
        return Err(config(format!(
            "R2_TEMP_CREDENTIALS_TTL must be between 1 and {MAX_TTL_SECONDS}"
        )));
    }
    Ok(value)
}

enum MintFailure {
    Rejected(String),
    Fatal(PublishError),
}

impl From<PublishError> for MintFailure {
    fn from(error: PublishError) -> Self {
        MintFailure::Fatal(error)
    }
}

fn mint(
    cf: &Cf,
    bucket: &str,
    parent: &str,
    account: &str,
) -> Result<Credentials, MintFailure> {
    let ttl = ttl_seconds(cf.env)?.to_string();
    let output = run_cf(
        cf,
        &[
            "r2",
            "temporary-credentials",
            "create",
            "--parent-access-key-id",
            parent,
            "--bucket",
            bucket,
            "--permission",
            "object-read-write",
            "--ttl-seconds",
            &ttl,
        ],
    )
    .map_err(MintFailure::Rejected)?;
    let document: Value = deser_json::from_str(&output)
        .map_err(|_| MintFailure::Fatal(config("cf returned credentials that are not valid JSON")))?;
    let field = |name: &str| {
        document
            .as_map()
            .and_then(|map| map.get(name))
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| MintFailure::Fatal(config(format!("cf returned no {name}"))))
    };
    Ok(Credentials {
        endpoint: format!("https://{account}.r2.cloudflarestorage.com"),
        access_key_id: field("accessKeyId")?,
        secret_access_key: field("secretAccessKey")?,
        region: "auto".to_string(),
        session_token: Some(field("sessionToken")?),
    })
}

fn run_cf(cf: &Cf, args: &[&str]) -> Result<String, String> {
    let env = cf.env;
    let program = find_program(env.get("PATH").unwrap_or_default(), "cf").ok_or_else(|| {
        format!("cf is not on PATH ({})", cf.login_hint())
    })?;
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        let output = Command::new(&program)
            .args(["--profile", cf.profile])
            .args(args)
            .env_clear()
            .envs(env.variables())
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("could not run cf: {error}"))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() { stderr.trim() } else { stdout.trim() };
        last = if detail.is_empty() {
            format!("{} failed", args.iter().take(3).copied().collect::<Vec<_>>().join(" "))
        } else {
            detail.to_string()
        };
        let rate_limited =
            last.contains("429") || last.to_lowercase().contains("rate limit");
        if rate_limited && attempt < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(1 << attempt));
            continue;
        }
        return Err(last);
    }
    Err(last)
}

fn find_program(path: &str, program: &str) -> Option<PathBuf> {
    path.split(':')
        .map(|entry| if entry.is_empty() { "." } else { entry })
        .map(|entry| Path::new(entry).join(program))
        .find(|candidate| candidate.is_file())
}

fn config(detail: impl Into<String>) -> PublishError {
    PublishError::Config(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_credentials_skip_any_cf_lookup() {
        let env = Environment::from_pairs([
            ("R2_S3_ENDPOINT", "https://account.r2.cloudflarestorage.com/"),
            ("R2_S3_ACCESS_KEY_ID", "access"),
            ("R2_S3_SECRET_ACCESS_KEY", "secret"),
            ("R2_S3_SESSION_TOKEN", "token"),
            ("PATH", "/nonexistent"),
        ]);
        let credentials = resolve(&env, "stuff", "bucket").unwrap();
        assert_eq!(
            credentials,
            Credentials {
                endpoint: "https://account.r2.cloudflarestorage.com".to_string(),
                access_key_id: "access".to_string(),
                secret_access_key: "secret".to_string(),
                region: "auto".to_string(),
                session_token: Some("token".to_string()),
            }
        );
    }

    #[test]
    fn a_partial_direct_environment_falls_back_to_minting() {
        let env = Environment::from_pairs([("R2_S3_ENDPOINT", "https://example")]);
        let error = resolve(&env, "stuff", "bucket").unwrap_err();
        assert!(error.to_string().contains("cf"), "{error}");
    }
}
