#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DIAGNOSTIC_LIMIT: usize = 16 * 1024;
const KILL_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct ProcessOptions {
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub stdout: String,
    pub stderr: String,
}

fn append_tail(current: &mut Vec<u8>, next: &[u8]) {
    current.extend_from_slice(next);
    if current.len() > DIAGNOSTIC_LIMIT {
        let excess = current.len() - DIAGNOSTIC_LIMIT;
        current.drain(..excess);
    }
}

fn signal_group(pid: u32, sig: &str) {
    let target = format!("-{pid}");
    let _ = Command::new("/bin/kill")
        .args([sig, &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn drain_available(
    rx: &mpsc::Receiver<Vec<u8>>,
    buf: &mut Vec<u8>,
) {
    while let Ok(chunk) = rx.try_recv() {
        append_tail(buf, &chunk);
    }
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
) -> Result<(bool, Option<i32>)> {
    loop {
        drain_available(rx_out, stdout);
        drain_available(rx_err, stderr);
        if let Some(status) = child.try_wait().map_err(|e| Error::Operation(format!("could not wait for {command}: {e}")))? {
            return Ok((false, status.code()));
        }
        if Instant::now() >= deadline {
            signal_group(pid, "-TERM");
            let grace_end = Instant::now() + KILL_GRACE;
            loop {
                drain_available(rx_out, stdout);
                drain_available(rx_err, stderr);
                if let Some(status) = child.try_wait().map_err(|e| Error::Operation(format!("could not wait for {command}: {e}")))? {
                    return Ok((true, status.code()));
                }
                if Instant::now() >= grace_end {
                    signal_group(pid, "-KILL");
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
    });

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let (timed_out, exit_code) = wait_for_exit(&mut child, &rx_out, &rx_err, &mut stdout, &mut stderr, pid, command, deadline)?;
    while let Ok(chunk) = rx_out.try_recv() {
        append_tail(&mut stdout, &chunk);
    }
    while let Ok(chunk) = rx_err.try_recv() {
        append_tail(&mut stderr, &chunk);
    }
    let _ = out_handle.join();
    let _ = err_handle.join();

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
    fn enforces_timeout() {
        let mut options = opts();
        options.timeout_ms = 200;
        let err = run_process("/bin/sleep", &["30".to_string()], &options).unwrap_err();
        assert!(format!("{err}").contains("time limit"));
    }
}
