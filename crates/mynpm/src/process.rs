#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DIAGNOSTIC_LIMIT: usize = 16 * 1024;
const KILL_GRACE: Duration = Duration::from_secs(5);
const READER_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
pub enum StdoutMode {
    Tail(usize),
    Full { max_bytes: u64 },
}

#[derive(Debug, Clone)]
pub struct ProcessOptions {
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
    pub stdout_mode: StdoutMode,
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub stdout: String,
    pub stderr: String,
}

fn append_tail(current: &mut Vec<u8>, next: &[u8], mode: StdoutMode, command: &str) -> Result<()> {
    match mode {
        StdoutMode::Tail(limit) => {
            current.extend_from_slice(next);
            if current.len() > limit {
                let excess = current.len() - limit;
                current.drain(..excess);
            }
            Ok(())
        }
        StdoutMode::Full { max_bytes } => {
            current.extend_from_slice(next);
            if current.len() as u64 > max_bytes {
                return Err(Error::Operation(format!(
                    "{command} output exceeds the {} limit",
                    crate::limits::format_bytes(max_bytes)
                )));
            }
            Ok(())
        }
    }
}

/// Sends `signal` to every member of process group `pgid` through
/// `/bin/kill` (first-party code forbids `unsafe`, so no `killpg`). The `--`
/// keeps procps from parsing the negative group as an option. Returns
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
        std::thread::sleep(Duration::from_millis(10));
    }
    signal_group(pgid, "KILL");
}

fn drain_available(
    rx: &mpsc::Receiver<Vec<u8>>,
    buf: &mut Vec<u8>,
    mode: StdoutMode,
    command: &str,
    is_stdout: bool,
) -> Result<()> {
    while let Ok(chunk) = rx.try_recv() {
        if is_stdout {
            append_tail(buf, &chunk, mode, command)?;
        } else {
            append_tail(buf, &chunk, StdoutMode::Tail(DIAGNOSTIC_LIMIT), command)?;
        }
    }
    Ok(())
}

fn await_readers(done: &mpsc::Receiver<()>) -> bool {
    let deadline = Instant::now() + READER_GRACE;
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || done.recv_timeout(remaining).is_err() {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn wait_for_exit(
    child: &mut std::process::Child,
    rx_out: &mpsc::Receiver<Vec<u8>>,
    rx_err: &mpsc::Receiver<Vec<u8>>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    pid: u32,
    command: &str,
    deadline: Instant,
    mode: StdoutMode,
) -> Result<(bool, Option<i32>)> {
    loop {
        drain_available(rx_out, stdout, mode, command, true)?;
        drain_available(rx_err, stderr, mode, command, false)?;
        if let Some(status) = child.try_wait().map_err(|e| Error::Operation(format!("could not wait for {command}: {e}")))? {
            return Ok((false, status.code()));
        }
        if Instant::now() >= deadline {
            signal_group(pid, "TERM");
            let grace_end = Instant::now() + KILL_GRACE;
            loop {
                drain_available(rx_out, stdout, mode, command, true)?;
                drain_available(rx_err, stderr, mode, command, false)?;
                if let Some(status) = child.try_wait().map_err(|e| Error::Operation(format!("could not wait for {command}: {e}")))? {
                    return Ok((true, status.code()));
                }
                if Instant::now() >= grace_end {
                    signal_group(pid, "KILL");
                    let _ = child.kill();
                    let status = child.wait().map_err(|e| Error::Operation(format!("could not wait for {command}: {e}")))?;
                    return Ok((true, status.code()));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn run_process(command: &str, args: &[String], options: &ProcessOptions) -> Result<ProcessResult> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .current_dir(&options.cwd)
        .env_clear()
        .envs(&options.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Operation(format!("could not start {command}: {e}")))?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let (tx_out, rx_out) = mpsc::channel::<Vec<u8>>();
    let (tx_err, rx_err) = mpsc::channel::<Vec<u8>>();
    let (tx_done, rx_done) = mpsc::channel::<()>();
    let out_done = tx_done.clone();
    let out_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        if let Some(pipe) = stdout_pipe.as_mut() {
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx_out.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = out_done.send(());
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        if let Some(pipe) = stderr_pipe.as_mut() {
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx_err.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = tx_done.send(());
    });

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mode = options.stdout_mode;
    let (timed_out, exit_code) = match wait_for_exit(&mut child, &rx_out, &rx_err, &mut stdout, &mut stderr, pid, command, deadline, mode) {
        Ok(values) => values,
        Err(e) => {
            signal_group(pid, "KILL");
            let _ = child.kill();
            let _ = child.wait();
            terminate_group(pid);
            if await_readers(&rx_done) {
                let _ = out_handle.join();
                let _ = err_handle.join();
            }
            return Err(e);
        }
    };
    terminate_group(pid);
    let readers_done = await_readers(&rx_done);
    while let Ok(chunk) = rx_out.try_recv() {
        append_tail(&mut stdout, &chunk, mode, command)?;
    }
    while let Ok(chunk) = rx_err.try_recv() {
        append_tail(&mut stderr, &chunk, StdoutMode::Tail(DIAGNOSTIC_LIMIT), command)?;
    }
    if readers_done {
        let _ = out_handle.join();
        let _ = err_handle.join();
    }

    if timed_out {
        return Err(Error::Operation(format!(
            "{command} exceeded its {}ms time limit",
            options.timeout_ms
        )));
    }
    match exit_code {
        Some(0) => Ok(ProcessResult {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }),
        Some(code) => {
            let diagnostic = String::from_utf8_lossy(&stderr).trim().to_string();
            let diagnostic = if diagnostic.is_empty() {
                String::from_utf8_lossy(&stdout).trim().to_string()
            } else {
                diagnostic
            };
            Err(Error::Operation(format!(
                "{command} exited with status {code}{}",
                if diagnostic.is_empty() { String::new() } else { format!(": {diagnostic}") }
            )))
        }
        None => Err(Error::Operation(format!("{command} terminated by signal"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ProcessOptions {
        ProcessOptions {
            cwd: "/tmp".to_string(),
            env: HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
            timeout_ms: 10_000,
            stdout_mode: StdoutMode::Tail(16384),
        }
    }

    #[test]
    fn runs_echo() {
        let result = run_process("/bin/echo", &["hi".to_string()], &opts()).unwrap();
        assert_eq!(result.stdout, "hi\n");
    }

    #[test]
    fn reports_nonzero_with_diagnostic() {
        let err = run_process("/bin/sh", &["-c".to_string(), "echo oops >&2; exit 3".to_string()], &opts()).unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("exited with status 3"), "{message}");
        assert!(message.contains("oops"), "{message}");
    }

    #[test]
    fn output_overflow_kills_the_child_before_joining_readers() {
        let mut options = opts();
        options.stdout_mode = StdoutMode::Full { max_bytes: 1024 };
        options.timeout_ms = 60_000;
        let started = Instant::now();
        let err = run_process(
            "/bin/sh",
            &["-c".to_string(), "yes | head -c 100000; sleep 30".to_string()],
            &options,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("output exceeds"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    fn assert_gone(pid_file: &std::path::Path) {
        let pid = std::fs::read_to_string(pid_file).unwrap().trim().to_string();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Command::new("/bin/kill")
            .args(["-s", "0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(Instant::now() < deadline, "owned descendant {pid} survived");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn shell(script: String, options: &ProcessOptions) -> Result<ProcessResult> {
        run_process("/bin/sh", &["-c".to_string(), script], options)
    }

    #[test]
    fn a_descendant_holding_the_pipes_does_not_outlive_an_exited_parent() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let started = Instant::now();
        let result = shell(format!("echo parent; sleep 30 & echo $! > {}; exit 0", pid_file.display()), &opts()).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(result.stdout, "parent\n");
        assert_gone(&pid_file);
    }

    #[test]
    fn a_descendant_ignoring_term_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let started = Instant::now();
        shell(format!("(trap '' TERM; sleep 30) & echo $! > {}; exit 0", pid_file.display()), &opts()).unwrap();
        assert!(started.elapsed() < KILL_GRACE + Duration::from_secs(3), "{:?}", started.elapsed());
        assert_gone(&pid_file);
    }

    #[test]
    fn a_timeout_kills_every_owned_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut options = opts();
        options.timeout_ms = 300;
        let err = shell(format!("sleep 30 & echo $! > {}; sleep 30", pid_file.display()), &options).unwrap_err();
        assert!(format!("{err}").contains("time limit"), "{err}");
        assert_gone(&pid_file);
    }

    #[test]
    fn output_overflow_kills_every_owned_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut options = opts();
        options.stdout_mode = StdoutMode::Full { max_bytes: 1024 };
        let err = shell(
            format!("sleep 30 & echo $! > {}; yes | head -c 100000; sleep 30", pid_file.display()),
            &options,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("output exceeds"), "{err}");
        assert_gone(&pid_file);
    }

    #[test]
    fn enforces_timeout() {
        let mut options = opts();
        options.timeout_ms = 200;
        let err = run_process("/bin/sleep", &["30".to_string()], &options).unwrap_err();
        assert!(format!("{err}").contains("time limit"));
    }
}
