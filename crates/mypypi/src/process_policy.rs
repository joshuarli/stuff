#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};
use rustc_hash::FxHashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CAPTURE_LIMIT: usize = 1024 * 1024;
const READ_CHUNK: usize = 16 * 1024;
const WAIT_POLL: Duration = Duration::from_millis(10);
const KILL_GRACE: Duration = Duration::from_secs(5);
const READER_GRACE: Duration = Duration::from_secs(1);

const ALLOWED_VARIABLES: &[&str] = &[
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "ARCHFLAGS",
    "CC",
    "CXX",
    "AR",
    "RANLIB",
    "LD",
    "STRIP",
    "CFLAGS",
    "CPPFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "LIBRARY_PATH",
    "MACOSX_DEPLOYMENT_TARGET",
    "SDKROOT",
    "DEVELOPER_DIR",
    "PKG_CONFIG_PATH",
    "PKG_CONFIG_LIBDIR",
    "SOURCE_DATE_EPOCH",
];

fn allowlisted(base: &FxHashMap<String, String>) -> FxHashMap<String, String> {
    base.iter()
        .filter(|(k, _)| ALLOWED_VARIABLES.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn uv_environment(source: Option<&FxHashMap<String, String>>) -> FxHashMap<String, String> {
    match source {
        Some(map) => allowlisted(map),
        None => allowlisted(&std::env::vars().collect()),
    }
}

pub fn scratch_environment(
    source: Option<&FxHashMap<String, String>>,
    scratch: &Path,
) -> FxHashMap<String, String> {
    let mut env = uv_environment(source);
    let scratch = scratch.display().to_string();
    for name in ["HOME", "TMPDIR", "TEMP", "TMP"] {
        env.insert(name.to_string(), scratch.clone());
    }
    env
}

fn source_path(source: &FxHashMap<String, String>, name: &str) -> Option<PathBuf> {
    source
        .get(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn absolute_path(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

fn source_home(source: &FxHashMap<String, String>) -> Option<PathBuf> {
    source_path(source, "HOME")
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .map(absolute_path)
}

fn source_toolchain_home(
    source: &FxHashMap<String, String>,
    name: &str,
    fallback: &str,
) -> Option<PathBuf> {
    source_path(source, name)
        .or_else(|| source_home(source).map(|home| home.join(fallback)))
        .map(absolute_path)
}

fn link_toolchain_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    match std::os::unix::fs::symlink(target, link) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn seed_rustup_home(origin: &Path, scratch: &Path) -> std::io::Result<()> {
    let toolchains = origin.join("toolchains");
    if toolchains.is_dir() {
        let destination = scratch.join("toolchains");
        std::fs::create_dir_all(&destination)?;
        for entry in std::fs::read_dir(&toolchains)? {
            let entry = entry?;
            link_toolchain_dir(&entry.path(), &destination.join(entry.file_name()))?;
        }
    }
    let settings = origin.join("settings.toml");
    if settings.is_file() {
        std::fs::create_dir_all(scratch)?;
        std::fs::copy(settings, scratch.join("settings.toml"))?;
    }
    Ok(())
}

pub(crate) fn seed_toolchain_home(
    source: &FxHashMap<String, String>,
    scratch_home: &Path,
) -> std::io::Result<()> {
    if let Some(cargo_home) = source_toolchain_home(source, "CARGO_HOME", ".cargo") {
        let bin = cargo_home.join("bin");
        if bin.is_dir() {
            let scratch_bin = scratch_home.join(".cargo/bin");
            std::fs::create_dir_all(&scratch_bin)?;
            for entry in std::fs::read_dir(&bin)? {
                let entry = entry?;
                link_toolchain_dir(&entry.path(), &scratch_bin.join(entry.file_name()))?;
            }
        }
    }
    if let Some(rustup_home) = source_toolchain_home(source, "RUSTUP_HOME", ".rustup") {
        seed_rustup_home(&rustup_home, &scratch_home.join(".rustup"))?;
    }
    Ok(())
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

/// Sends `signal` to every member of process group `pgid` through
/// `/bin/kill` (first-party code forbids `unsafe`, so no `killpg`). Returns
/// whether any member received it.
fn signal_group(pgid: u32, signal: &str) -> bool {
    Command::new("/bin/kill")
        .args(["-s", signal, "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Terminates every remaining member of the child's process group: TERM,
/// then KILL after `KILL_GRACE`. Runs after the direct child exits too,
/// because descendants that outlive it still own its pipes.
fn terminate_group(pgid: u32) {
    if !signal_group(pgid, "TERM") {
        return;
    }
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if !signal_group(pgid, "0") {
            return;
        }
        std::thread::sleep(WAIT_POLL);
    }
    signal_group(pgid, "KILL");
}

fn stop_process_group(child: &mut std::process::Child) {
    let pgid = child.id();
    signal_group(pgid, "TERM");
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(WAIT_POLL);
    }
    signal_group(pgid, "KILL");
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_reader<R: Read + Send + 'static>(pipe: Option<R>, sender: mpsc::Sender<Vec<u8>>) {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if sender.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

fn append_capped(captured: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= CAPTURE_LIMIT {
        captured.clear();
        captured.extend_from_slice(&chunk[chunk.len() - CAPTURE_LIMIT..]);
        return;
    }
    let excess = (captured.len() + chunk.len()).saturating_sub(CAPTURE_LIMIT);
    if excess > 0 {
        captured.drain(..excess);
    }
    captured.extend_from_slice(chunk);
}

fn drain_available(receiver: &mpsc::Receiver<Vec<u8>>, captured: &mut Vec<u8>) {
    while let Ok(chunk) = receiver.try_recv() {
        append_capped(captured, &chunk);
    }
}

/// Collects the remaining output until both readers reach EOF or
/// `READER_GRACE` passes. A process outside the group can hold a pipe open
/// forever; its reader thread is then left detached and the partial output
/// is kept.
fn finish_readers(
    rx_out: &mpsc::Receiver<Vec<u8>>,
    rx_err: &mpsc::Receiver<Vec<u8>>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) {
    let deadline = Instant::now() + READER_GRACE;
    let mut open = [true, true];
    while open.iter().any(|open| *open) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        for (index, (receiver, captured)) in
            [(rx_out, &mut *stdout), (rx_err, &mut *stderr)].into_iter().enumerate()
        {
            if !open[index] {
                continue;
            }
            match receiver.recv_timeout(remaining.min(WAIT_POLL)) {
                Ok(chunk) => append_capped(captured, &chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => open[index] = false,
            }
        }
    }
    drain_available(rx_out, stdout);
    drain_available(rx_err, stderr);
}

fn decode(captured: &[u8]) -> String {
    String::from_utf8_lossy(captured).into_owned()
}

pub fn run_capture(
    command: &[String],
    cwd: Option<&Path>,
    env: &FxHashMap<String, String>,
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
            let data = data.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&data);
            });
        }
    }
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    spawn_reader(child.stdout.take(), tx_out);
    spawn_reader(child.stderr.take(), tx_err);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    loop {
        drain_available(&rx_out, &mut stdout);
        drain_available(&rx_err, &mut stderr);
        match child.try_wait().map_err(|e| RunError::Spawn(e.to_string()))? {
            Some(status) => {
                terminate_group(child.id());
                finish_readers(&rx_out, &rx_err, &mut stdout, &mut stderr);
                return Ok(Captured {
                    status_code: status.code().unwrap_or(-1),
                    stdout: decode(&stdout),
                    stderr: decode(&stderr),
                });
            }
            None => {
                if Instant::now() >= deadline {
                    stop_process_group(&mut child);
                    terminate_group(child.id());
                    finish_readers(&rx_out, &rx_err, &mut stdout, &mut stderr);
                    return Err(RunError::Timeout {
                        stdout: decode(&stdout),
                        stderr: decode(&stderr),
                    });
                }
                std::thread::sleep(WAIT_POLL);
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

    fn test_env() -> FxHashMap<String, String> {
        [("PATH".to_string(), "/usr/bin:/bin".to_string())].into_iter().collect()
    }

    #[test]
    fn environment_allowlist_excludes_publisher_secrets() {
        let mut env = FxHashMap::default();
        for name in ["PATH", "SSL_CERT_FILE", "LANG", "CFLAGS"] {
            env.insert(name.to_string(), format!("{name}-value"));
        }
        for name in [
            "UV_INDEX",
            "PIP_INDEX_URL",
            "PYTHONPATH",
            "R2_S3_ACCESS_KEY_ID",
            "R2_S3_SECRET_ACCESS_KEY",
            "R2_S3_SESSION_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GITHUB_TOKEN",
            "PYPI_TOKEN",
            "HOME",
            "TMPDIR",
        ] {
            env.insert(name.to_string(), "publisher-secret".to_string());
        }
        let filtered = uv_environment(Some(&env));
        assert_eq!(filtered.get("PATH").map(|s| s.as_str()), Some("PATH-value"));
        assert_eq!(filtered.get("SSL_CERT_FILE").map(|s| s.as_str()), Some("SSL_CERT_FILE-value"));
        for name in [
            "UV_INDEX",
            "PIP_INDEX_URL",
            "PYTHONPATH",
            "R2_S3_ACCESS_KEY_ID",
            "R2_S3_SECRET_ACCESS_KEY",
            "R2_S3_SESSION_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GITHUB_TOKEN",
            "PYPI_TOKEN",
            "HOME",
            "TMPDIR",
        ] {
            assert!(!filtered.contains_key(name), "{name} leaked into the subprocess environment");
        }
    }

    #[test]
    fn scratch_environment_redirects_home_and_temp() {
        let mut env = FxHashMap::default();
        env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        env.insert("HOME".to_string(), "/home/publisher".to_string());
        env.insert("TMPDIR".to_string(), "/home/publisher/tmp".to_string());
        let scratch = Path::new("/scratch/build");
        let isolated = scratch_environment(Some(&env), scratch);
        for name in ["HOME", "TMPDIR", "TEMP", "TMP"] {
            assert_eq!(isolated.get(name).map(|s| s.as_str()), Some("/scratch/build"), "{name}");
        }
        assert_eq!(isolated.get("PATH").map(|s| s.as_str()), Some("/usr/bin:/bin"));
    }

    #[test]
    fn redaction_hides_credentials_and_keeps_the_tail_on_char_boundaries() {
        let v = redacted_diagnostic("fetch https://user:secret@example.test/simple failed", 2000);
        assert!(v.contains("https://[credentials]@example.test/simple"));
        assert!(!v.contains("secret"));
        assert_eq!(redacted_diagnostic("abcdef", 3), "def");
        assert_eq!(redacted_diagnostic("échec", 4096), "échec");
        assert_eq!(redacted_diagnostic("échec", 3), "hec");
        assert_eq!(redacted_diagnostic(&"é".repeat(10), 5), "éé");
    }

    #[test]
    fn drains_output_larger_than_the_pipe_buffer() {
        let script = "yes out | head -c 204800; yes err | head -c 204800 1>&2".to_string();
        let out = run_capture(
            &["/bin/sh".to_string(), "-c".to_string(), script],
            None,
            &test_env(),
            None,
            10,
        )
        .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout.len(), 204800);
        assert_eq!(out.stderr.len(), 204800);
    }

    #[test]
    fn caps_each_captured_stream() {
        let bytes = CAPTURE_LIMIT + 4096;
        let script = format!("yes out | head -c {bytes}; yes err | head -c {bytes} 1>&2");
        let out = run_capture(
            &["/bin/sh".to_string(), "-c".to_string(), script],
            None,
            &test_env(),
            None,
            10,
        )
        .unwrap();
        assert_eq!(out.stdout.len(), CAPTURE_LIMIT);
        assert_eq!(out.stderr.len(), CAPTURE_LIMIT);
    }

    #[test]
    fn retains_non_utf8_output_lossily() {
        let out = run_capture(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf '\\377\\376'; printf '\\377' 1>&2".to_string(),
            ],
            None,
            &test_env(),
            None,
            10,
        )
        .unwrap();
        assert_eq!(out.stdout, "\u{FFFD}\u{FFFD}");
        assert_eq!(out.stderr, "\u{FFFD}");
    }

    #[test]
    fn times_out_and_kills_sleeping_child() {
        let started = Instant::now();
        let err = run_capture(
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo before-timeout; sleep 30".to_string(),
            ],
            None,
            &test_env(),
            None,
            1,
        )
        .unwrap_err();
        match err {
            RunError::Timeout { stdout, .. } => assert!(stdout.contains("before-timeout")),
            other => panic!("expected timeout, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    fn shell(script: &str, timeout_secs: u64) -> std::result::Result<Captured, RunError> {
        run_capture(
            &["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
            None,
            &test_env(),
            None,
            timeout_secs,
        )
    }

    fn assert_gone(pid_file: &Path) {
        let pid: u32 = std::fs::read_to_string(pid_file).unwrap().trim().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Command::new("/bin/kill")
            .args(["-s", "0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(Instant::now() < deadline, "owned descendant {pid} survived");
            std::thread::sleep(WAIT_POLL);
        }
    }

    #[test]
    fn a_descendant_holding_the_pipes_does_not_outlive_an_exited_parent() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let started = Instant::now();
        let out = shell(
            &format!("echo parent; sleep 30 & echo $! > {}; exit 0", pid_file.display()),
            60,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(out.status_code, 0);
        assert_eq!(out.stdout, "parent\n");
        assert_gone(&pid_file);
    }

    #[test]
    fn a_descendant_ignoring_term_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let started = Instant::now();
        shell(
            &format!("(trap '' TERM; sleep 30) & echo $! > {}; exit 0", pid_file.display()),
            60,
        )
        .unwrap();
        assert!(started.elapsed() < KILL_GRACE + Duration::from_secs(3), "{:?}", started.elapsed());
        assert_gone(&pid_file);
    }

    #[test]
    fn a_timeout_kills_every_owned_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let err = shell(&format!("sleep 30 & echo $! > {}; sleep 30", pid_file.display()), 1)
            .unwrap_err();
        assert!(matches!(err, RunError::Timeout { .. }), "{err:?}");
        assert_gone(&pid_file);
    }
}
