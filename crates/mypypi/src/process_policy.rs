#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const POLICY_VARIABLES: &[&str] = &[
    "UV_DEFAULT_INDEX",
    "UV_FIND_LINKS",
    "UV_INDEX",
    "UV_INDEX_STRATEGY",
    "UV_NO_BUILD_ISOLATION",
    "UV_NO_BUILD_ISOLATION_PACKAGE",
    "UV_NO_CONFIG",
    "UV_NO_VERIFY_HASHES",
    "UV_OVERRIDE",
    "UV_BUILD_CONSTRAINT",
    "UV_BUILD_CONSTRAINTS",
    "UV_CONSTRAINT",
    "UV_CONFIG_FILE",
    "UV_INSECURE_HOST",
    "PIP_BUILD_CONSTRAINT",
    "PIP_CONFIG_FILE",
    "PIP_CONSTRAINT",
    "PIP_EXTRA_INDEX_URL",
    "PIP_FIND_LINKS",
    "PIP_INDEX_URL",
    "PIP_TRUSTED_HOST",
    "PYTHONHOME",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONUSERBASE",
];

fn is_policy_variable(name: &str) -> bool {
    if POLICY_VARIABLES.contains(&name) {
        return true;
    }
    name.starts_with("UV_INDEX_")
        || name.starts_with("UV_EXTRA_INDEX")
        || name.starts_with("PIP_INDEX_")
        || name.starts_with("PIP_EXTRA_INDEX")
}

pub fn uv_environment(source: Option<&BTreeMap<String, String>>) -> BTreeMap<String, String> {
    let owned;
    let base: &BTreeMap<String, String> = match source {
        Some(m) => m,
        None => {
            owned = std::env::vars().collect::<BTreeMap<_, _>>();
            // borrow of a temporary avoided by scoping below
            return strip_map(&owned);
        }
    };
    strip_map(base)
}

fn strip_map(base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    base.iter()
        .filter(|(k, _)| !is_policy_variable(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn redacted_diagnostic(value: &str, limit: usize) -> String {
    let redacted = redact_credentials(value);
    if redacted.len() <= limit {
        return redacted;
    }
    let start = redacted.len() - limit;
    let start = (start..=redacted.len())
        .find(|i| redacted.is_char_boundary(*i))
        .unwrap_or(redacted.len());
    redacted[start..].to_string()
}

fn redact_credentials(value: &str) -> String {
    redact_credentials_clean(value)
}

fn redact_credentials_clean(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    let b = value.as_bytes();
    while i < b.len() {
        let rest = &value[i..];
        let scheme_len = if rest.starts_with("https://") {
            8usize
        } else if rest.starts_with("http://") {
            7usize
        } else {
            let ch = rest.chars().next().expect("non-empty remainder");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        };
        out.push_str(&rest[..scheme_len]);
        let after_start = i + scheme_len;
        let after = &value[after_start..];
        let end_rel = after
            .find(['/', '?', '#', ' ', '\n', '\t', '"', '\''])
            .unwrap_or(after.len());
        let authority = &after[..end_rel];
        if let Some(at) = authority.find('@') {
            out.push_str("[credentials]@");
            out.push_str(&authority[at + 1..]);
        } else {
            out.push_str(authority);
        }
        i = after_start + end_rel;
    }
    out
}

#[derive(Debug, Clone)]
pub struct Captured {
    pub status_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.status_code == 0
    }
}

#[derive(Debug)]
pub enum RunError {
    Spawn(String),
    Timeout { stdout: String, stderr: String },
    Interrupted,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "could not spawn process: {e}"),
            RunError::Timeout { .. } => write!(f, "process timed out"),
            RunError::Interrupted => write!(f, "process interrupted"),
        }
    }
}

impl std::error::Error for RunError {}

fn signal_group(pid: u32, signal: &str) {
    let target = format!("-{pid}");
    let _ = Command::new("/bin/kill")
        .args([signal, &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn stop_process_group(child: &mut std::process::Child) {
    let pid = child.id();
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    signal_group(pid, "-TERM");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if child.try_wait().ok().flatten().is_none() {
        signal_group(pid, "-KILL");
        let _ = child.wait();
    }
    let _ = child.kill();
}

pub fn run_capture(
    command: &[String],
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
    input: Option<&[u8]>,
    timeout_secs: u64,
) -> std::result::Result<Captured, RunError> {
    if command.is_empty() {
        return Err(RunError::Spawn("empty command".to_string()));
    }
    let mut cmd = Command::new(&command[0]);
    cmd.args(&command[1..]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Detach into a fresh process group so a timeout can terminate the whole
    // tree (equivalent of start_new_session + killpg, without libc).
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| RunError::Spawn(e.to_string()))?;
    if let Some(data) = input {
        use std::io::Write as _;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    loop {
        match child.try_wait().map_err(|e| RunError::Spawn(e.to_string()))? {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut o) = child.stdout.take() {
                    let _ = o.read_to_string(&mut stdout);
                }
                if let Some(mut e) = child.stderr.take() {
                    let _ = e.read_to_string(&mut stderr);
                }
                return Ok(Captured {
                    status_code: status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                });
            }
            None => {
                if Instant::now() >= deadline {
                    stop_process_group(&mut child);
                    let mut stdout = String::new();
                    let mut stderr = String::new();
                    if let Some(mut o) = child.stdout.take() {
                        let _ = o.read_to_string(&mut stdout);
                    }
                    if let Some(mut e) = child.stderr.take() {
                        let _ = e.read_to_string(&mut stderr);
                    }
                    // Reap fully.
                    let _ = child.wait();
                    return Err(RunError::Timeout { stdout, stderr });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

pub fn check_captured(out: Captured, what: &str, stage: Stage) -> Result<Captured> {
    if out.success() {
        Ok(out)
    } else {
        Err(Error::new(
            Kind::Dependency,
            format!("{what} failed with exit {}", out.status_code),
            stage,
        ))
    }
}

/// Create a uniquely-named temporary directory (std-only; `tempfile` is a
/// dev-dependency and unavailable in the binary).
pub fn make_temp_dir(prefix: &str) -> std::io::Result<std::path::PathBuf> {
    for counter in 0..100u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("{prefix}{}-{nanos}-{counter}", std::process::id());
        let path = std::env::temp_dir().join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create temporary directory",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_policy_variables() {
        let mut env = BTreeMap::new();
        env.insert("UV_INDEX".to_string(), "x".to_string());
        env.insert("UV_EXTRA_INDEX_URL".to_string(), "x".to_string());
        env.insert("PIP_INDEX_URL".to_string(), "x".to_string());
        env.insert("PATH".to_string(), "/bin".to_string());
        env.insert("CARGO_HOME".to_string(), "/tmp".to_string());
        let stripped = uv_environment(Some(&env));
        assert!(!stripped.contains_key("UV_INDEX"));
        assert!(!stripped.contains_key("UV_EXTRA_INDEX_URL"));
        assert!(!stripped.contains_key("PIP_INDEX_URL"));
        assert_eq!(stripped.get("PATH").map(|s| s.as_str()), Some("/bin"));
    }

    #[test]
    fn redacts_userinfo() {
        let v = redacted_diagnostic("fetch https://user:secret@example.test/simple failed", 2000);
        assert!(v.contains("https://[credentials]@example.test/simple"));
        assert!(!v.contains("secret"));
    }

    #[test]
    fn redaction_respects_limit_tail() {
        let v = redacted_diagnostic("abcdef", 3);
        assert_eq!(v, "def");
    }

    #[test]
    fn redaction_handles_multibyte_text() {
        assert_eq!(redacted_diagnostic("échec", 4096), "échec");
        assert_eq!(redacted_diagnostic("échec", 3), "hec");
        let long = "é".repeat(10);
        assert_eq!(redacted_diagnostic(&long, 5), "éé");
    }

    #[test]
    fn runs_echo_and_times_out_sleep() {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let out = run_capture(
            &["/bin/echo".to_string(), "hi".to_string()],
            None,
            &env,
            None,
            10,
        )
        .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.trim(), "hi");
        let err = run_capture(&["/bin/sleep".to_string(), "30".to_string()], None, &env, None, 1)
            .unwrap_err();
        assert!(matches!(err, RunError::Timeout { .. }));
    }
}
