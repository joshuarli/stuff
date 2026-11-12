#![forbid(unsafe_code)]

use crate::error::{Error, Result, Stage};
use rustc_hash::FxHashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

const MACOS_SYSTEM_READ: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library",
    "/opt/homebrew",
    "/private/etc",
    "/private/var/select",
];

const LINUX_SYSTEM_READ: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt",
];

const CERTIFICATE_VARIABLES: &[&str] =
    &["SSL_CERT_FILE", "SSL_CERT_DIR", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE"];

const LINUX_RESOLVER: &str = "/run/systemd/resolve";

const HOME_CREDENTIALS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".docker",
    ".kube",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".local/share/uv/credentials",
    "Library/Keychains",
];

const INTERPRETER_PROBE: &str = "import os, sys, sysconfig as s; print(*[sys.base_prefix, sys.base_exec_prefix, os.path.realpath(sys.executable), s.get_path('stdlib'), s.get_path('platstdlib'), s.get_path('include'), s.get_path('platinclude')], sep='\\n')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Offline,
    Online,
}

/// Filesystem and network authority granted to one confined child. Nothing
/// outside `readable`, `writable`, and the operating system's own files is
/// visible to it.
#[derive(Debug, Clone)]
pub struct Boundary {
    pub writable: Vec<PathBuf>,
    pub readable: Vec<PathBuf>,
    pub network: Network,
}

#[derive(Debug, Clone)]
pub struct Interpreter {
    pub executable: PathBuf,
    pub roots: Vec<PathBuf>,
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// Finds an executable the way `execvp` would, without resolving symbolic
/// links: a virtual environment's interpreter must keep its own path.
pub fn resolve_program(name: &str, path: &str) -> Option<PathBuf> {
    let candidate = |candidate: &Path| -> Option<PathBuf> {
        let metadata = std::fs::metadata(candidate).ok()?;
        (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .then(|| std::path::absolute(candidate).unwrap_or_else(|_| candidate.to_path_buf()))
    };
    if name.contains('/') {
        return candidate(Path::new(name));
    }
    path.split(':')
        .filter(|directory| !directory.is_empty())
        .find_map(|directory| candidate(&Path::new(directory).join(name)))
}

fn probe_interpreter(executable: &Path) -> Vec<PathBuf> {
    let Ok(output) = Command::new(executable)
        .args(["-S", "-I", "-c", INTERPRETER_PROBE])
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != 7 || lines.iter().any(|line| !line.starts_with('/')) {
        return Vec::new();
    }
    lines
        .into_iter()
        .map(Path::new)
        .filter(|path| path.exists())
        .map(canonical)
        .collect()
}

/// Resolves `python` through `path` and collects the directories the
/// interpreter needs to start, so a confined child can run it without any
/// other part of the publisher's home.
pub fn interpreter(python: &str, path: &str, stage: Stage) -> Result<Interpreter> {
    let executable = resolve_program(python, path)
        .map(|found| canonical(&found))
        .ok_or_else(|| Error::dependency(format!("Python interpreter {python} was not found"), stage))?;
    let mut roots = probe_interpreter(&executable);
    if roots.is_empty()
        && let Some(prefix) = executable.parent().and_then(Path::parent)
    {
        roots.push(prefix.to_path_buf());
    }
    roots.push(executable.clone());
    Ok(Interpreter { executable, roots })
}

/// `xcodebuild`, which the `cc` shim runs, loads frameworks from beside the
/// selected developer directory, so the whole application bundle is readable.
fn developer_bundle(developer_dir: &Path) -> PathBuf {
    match developer_dir.ancestors().find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app")) {
        Some(bundle) => bundle.to_path_buf(),
        None => developer_dir.to_path_buf(),
    }
}

fn developer_roots(env: &FxHashMap<String, String>) -> Vec<PathBuf> {
    static XCODE: OnceLock<Option<PathBuf>> = OnceLock::new();
    let mut roots = Vec::new();
    if let Some(directory) = env.get("DEVELOPER_DIR").filter(|value| value.starts_with('/')) {
        roots.push(developer_bundle(&canonical(Path::new(directory))));
    }
    let selected = XCODE.get_or_init(|| {
        let output = Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let line = text.trim();
        (output.status.success() && line.starts_with('/'))
            .then(|| developer_bundle(&canonical(Path::new(line))))
    });
    roots.extend(selected.clone());
    roots
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| {
            Error::invalid_input(
                format!("execution boundary path is not printable UTF-8: {}", path.display()),
                Stage::Command,
            )
        })
}

fn sbpl_string(path: &Path) -> Result<String> {
    let text = path_text(path)?;
    Ok(format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"")))
}

fn sbpl_subpaths<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Result<String> {
    let mut out = String::new();
    for path in paths {
        out.push_str(" (subpath ");
        out.push_str(&sbpl_string(path)?);
        out.push(')');
    }
    Ok(out)
}

/// `(deny default)` with `system.sb` for dyld, sysctl and mach basics. SBPL
/// takes the last matching rule, so the credential denials come last.
/// `file-read-metadata` stays open because uv stats the ancestors of its
/// working directory and treats EPERM there as fatal; directory listings and
/// file contents remain denied.
fn seatbelt_profile(
    boundary: &Boundary,
    system: &[PathBuf],
    home: Option<&Path>,
) -> Result<String> {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(import \"system.sb\")\n\
         (allow process-exec* process-fork)\n(allow signal (target same-sandbox))\n\
         (allow file-read-metadata)\n",
    );
    profile.push_str(&format!(
        "(allow file-read* file-map-executable{})\n",
        sbpl_subpaths(system.iter().chain(&boundary.readable))?
    ));
    profile.push_str(&format!(
        "(allow file-read* file-write* file-map-executable{})\n",
        sbpl_subpaths(&boundary.writable)?
    ));
    if boundary.network == Network::Online {
        profile.push_str("(allow network-outbound)\n");
    }
    if let Some(home) = home {
        let protected: Vec<PathBuf> = HOME_CREDENTIALS.iter().map(|name| home.join(name)).collect();
        profile.push_str(&format!(
            "(deny file-read* file-write* file-map-executable{})\n",
            sbpl_subpaths(&protected)?
        ));
    }
    Ok(profile)
}

fn bwrap_arguments(boundary: &Boundary, home: Option<&Path>) -> Result<Vec<String>> {
    let mut args: Vec<String> = [
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--cap-drop",
        "ALL",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if boundary.network == Network::Offline {
        args.push("--unshare-net".to_string());
    }
    let mut system: Vec<&str> = LINUX_SYSTEM_READ.to_vec();
    if boundary.network == Network::Online {
        system.push(LINUX_RESOLVER);
    }
    for root in &system {
        args.extend(["--ro-bind-try".to_string(), root.to_string(), root.to_string()]);
    }
    if let Some(home) = home {
        args.extend(["--tmpfs".to_string(), path_text(home)?.to_string()]);
    }
    for path in &boundary.readable {
        if LINUX_SYSTEM_READ.iter().any(|root| path.starts_with(root)) {
            continue;
        }
        let text = path_text(path)?;
        args.extend(["--ro-bind".to_string(), text.to_string(), text.to_string()]);
    }
    for path in &boundary.writable {
        let text = path_text(path)?;
        args.extend(["--bind".to_string(), text.to_string(), text.to_string()]);
    }
    args.push("--".to_string());
    Ok(args)
}

/// Every symbolic link a path resolves through, so a chain such as a virtual
/// environment interpreter linked to a versionless toolchain alias still
/// resolves inside the boundary.
fn link_hops(path: &Path) -> Vec<PathBuf> {
    const MAX_LINKS: usize = 40;
    let mut hops = Vec::new();
    let mut current = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    'resolve: while hops.len() < MAX_LINKS {
        let mut prefix = PathBuf::new();
        for component in current.components() {
            prefix.push(component);
            let Ok(target) = std::fs::read_link(&prefix) else {
                continue;
            };
            let rest = current.strip_prefix(&prefix).map(Path::to_path_buf).unwrap_or_default();
            let resolved = prefix.parent().map_or_else(|| target.clone(), |parent| parent.join(&target));
            hops.push(prefix);
            current = if rest.as_os_str().is_empty() { resolved } else { resolved.join(rest) };
            continue 'resolve;
        }
        break;
    }
    hops
}

fn publisher_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| canonical(&home))
}

fn refuse_home_exposure(boundary: &Boundary, home: Option<&Path>) -> Result<()> {
    let Some(home) = home else {
        return Ok(());
    };
    for grant in boundary.readable.iter().chain(&boundary.writable) {
        if home.starts_with(grant) {
            return Err(Error::configuration(format!(
                "execution boundary would expose the publisher home through {}",
                grant.display()
            )));
        }
    }
    Ok(())
}

fn find_bwrap() -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    resolve_program("bwrap", &path).or_else(|| resolve_program("/usr/bin/bwrap", ""))
}

fn tool() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(PathBuf::from(SANDBOX_EXEC))
    } else if cfg!(target_os = "linux") {
        find_bwrap().ok_or_else(|| {
            Error::dependency(
                "bubblewrap (bwrap) is required to run package code and was not found; install it (for example `apt install bubblewrap` or `apk add bubblewrap`)",
                Stage::Command,
            )
        })
    } else {
        Err(Error::dependency(
            "no execution boundary is implemented for this operating system",
            Stage::Command,
        ))
    }
}

fn wrap(
    tool: &Path,
    boundary: &Boundary,
    env: &FxHashMap<String, String>,
    home: Option<&Path>,
) -> Result<Vec<String>> {
    let mut argv = vec![path_text(tool)?.to_string()];
    if cfg!(target_os = "macos") {
        let mut system: Vec<PathBuf> = MACOS_SYSTEM_READ.iter().map(PathBuf::from).collect();
        system.extend(developer_roots(env));
        argv.push("-p".to_string());
        argv.push(seatbelt_profile(boundary, &system, home)?);
    } else {
        argv.extend(bwrap_arguments(boundary, home)?);
    }
    Ok(argv)
}

fn require_working_boundary() -> Result<PathBuf> {
    static PROBE: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();
    let probed = PROBE.get_or_init(|| {
        let tool = tool().map_err(|error| error.detail)?;
        let truth = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .find(|candidate| Path::new(candidate).exists())
            .unwrap_or("/usr/bin/true");
        let empty = Boundary { writable: Vec::new(), readable: Vec::new(), network: Network::Offline };
        let mut argv = wrap(&tool, &empty, &FxHashMap::default(), None).map_err(|e| e.detail)?;
        argv.push(truth.to_string());
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("could not start {}: {error}", tool.display()))?;
        if output.status.success() {
            Ok(tool)
        } else {
            Err(format!(
                "{} cannot create a sandbox here: {}",
                tool.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    });
    probed.clone().map_err(|reason| {
        Error::dependency(
            format!("execution boundary unavailable ({reason}); package code is not run without it"),
            Stage::Command,
        )
    })
}

/// Rewrites `command` so it runs inside `boundary`. Fails closed when the
/// platform's sandbox tool is missing or cannot start.
pub fn confine(
    command: &[String],
    env: &FxHashMap<String, String>,
    boundary: &Boundary,
    stage: Stage,
) -> Result<Vec<String>> {
    let with_stage = |mut error: Error| {
        error.stage = stage;
        error
    };
    let first = command
        .first()
        .ok_or_else(|| Error::invalid_input("empty command", stage))?;
    let path = env.get("PATH").map(String::as_str).unwrap_or("");
    let program = resolve_program(first, path)
        .ok_or_else(|| Error::dependency(format!("{first} was not found on PATH"), stage))?;
    let tool = require_working_boundary().map_err(with_stage)?;
    let mut confined = Boundary {
        writable: boundary.writable.iter().map(|path| canonical(path)).collect(),
        readable: boundary.readable.iter().map(|path| canonical(path)).collect(),
        network: boundary.network,
    };
    for name in CERTIFICATE_VARIABLES {
        if let Some(value) = env.get(*name).filter(|value| value.starts_with('/')) {
            confined.readable.push(canonical(Path::new(value)));
        }
    }
    confined.readable.push(canonical(&program));
    confined.readable.push(program.clone());
    let hops: Vec<PathBuf> = link_hops(&program)
        .into_iter()
        .filter(|hop| !confined.writable.iter().any(|root| hop.starts_with(root)))
        .collect();
    confined.readable.extend(hops);
    let home = publisher_home();
    refuse_home_exposure(&confined, home.as_deref()).map_err(with_stage)?;
    let mut argv = wrap(&tool, &confined, env, home.as_deref()).map_err(with_stage)?;
    argv.push(path_text(&program).map_err(with_stage)?.to_string());
    argv.extend(command[1..].iter().cloned());
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_policy::{run_capture, scratch_environment};

    fn boundary(writable: &Path, network: Network) -> Boundary {
        Boundary { writable: vec![writable.to_path_buf()], readable: Vec::new(), network }
    }

    #[test]
    fn seatbelt_profile_is_default_deny_and_names_only_granted_paths() {
        let profile = seatbelt_profile(
            &Boundary {
                writable: vec![PathBuf::from("/scratch/a\"b")],
                readable: vec![PathBuf::from("/opt/python")],
                network: Network::Offline,
            },
            &[PathBuf::from("/usr")],
            Some(Path::new("/home/publisher")),
        )
        .unwrap();
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(subpath \"/scratch/a\\\"b\")"));
        assert!(profile.contains("(subpath \"/opt/python\")"));
        assert!(!profile.contains("network"));
        let denial = profile.lines().last().unwrap();
        assert!(denial.starts_with("(deny file-read* file-write*"));
        assert!(denial.contains("\"/home/publisher/.ssh\""));
        assert!(denial.contains("\"/home/publisher/.config\""));
        let online = seatbelt_profile(
            &boundary(Path::new("/scratch"), Network::Online),
            &[],
            None,
        )
        .unwrap();
        assert!(online.contains("(allow network-outbound)"));
        assert!(!online.contains("(deny file-read*"));
    }

    #[test]
    fn bwrap_arguments_mount_only_system_directories_and_the_grants() {
        let args = bwrap_arguments(
            &Boundary {
                writable: vec![PathBuf::from("/tmp/scratch")],
                readable: vec![PathBuf::from("/home/publisher/.local/py"), PathBuf::from("/usr/local/py")],
                network: Network::Offline,
            },
            Some(Path::new("/home/publisher")),
        )
        .unwrap();
        let joined = args.join(" ");
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(joined.contains("--tmpfs /home/publisher --ro-bind /home/publisher/.local/py /home/publisher/.local/py"));
        assert!(joined.contains("--bind /tmp/scratch /tmp/scratch"));
        assert!(!joined.contains("--ro-bind /usr/local/py"));
        assert!(!args.contains(&"/".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("--"));
        let online = bwrap_arguments(&boundary(Path::new("/tmp/scratch"), Network::Online), None).unwrap();
        assert!(!online.contains(&"--unshare-net".to_string()));
        assert!(online.join(" ").contains(LINUX_RESOLVER));
    }

    #[test]
    fn a_grant_that_contains_the_publisher_home_is_refused() {
        let home = Path::new("/home/publisher");
        for grant in ["/", "/home", "/home/publisher"] {
            let bad = Boundary {
                writable: Vec::new(),
                readable: vec![PathBuf::from(grant)],
                network: Network::Offline,
            };
            assert!(refuse_home_exposure(&bad, Some(home)).is_err(), "{grant}");
        }
        let fine = Boundary {
            writable: vec![PathBuf::from("/home/publisher/.local/state/scratch")],
            readable: vec![PathBuf::from("/home/publisher/.local/share/py")],
            network: Network::Offline,
        };
        assert!(refuse_home_exposure(&fine, Some(home)).is_ok());
    }

    #[test]
    fn resolves_programs_through_the_given_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("/nonexistent:{}", dir.path().display());
        assert_eq!(resolve_program("tool", &path), Some(tool.clone()));
        assert_eq!(resolve_program("tool", "/nonexistent"), None);
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(resolve_program("tool", &path), None);
    }

    struct Probe {
        dir: tempfile::TempDir,
        env: FxHashMap<String, String>,
        interpreter: Interpreter,
    }

    fn probe() -> Probe {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().canonicalize().unwrap().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let mut env = scratch_environment(None, &scratch);
        let path = env.get("PATH").cloned().unwrap_or_default();
        let interpreter = interpreter("python3", &path, Stage::Validation).unwrap();
        env.insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());
        Probe { dir, env, interpreter }
    }

    impl Probe {
        fn scratch(&self) -> PathBuf {
            self.dir.path().canonicalize().unwrap().join("scratch")
        }

        fn run(&self, network: Network, code: &str) -> crate::process_policy::Captured {
            let boundary = Boundary {
                writable: vec![self.scratch()],
                readable: self.interpreter.roots.clone(),
                network,
            };
            let command = vec![
                self.interpreter.executable.display().to_string(),
                "-I".to_string(),
                "-c".to_string(),
                code.to_string(),
            ];
            let confined = confine(&command, &self.env, &boundary, Stage::Validation).unwrap();
            run_capture(&confined, Some(&self.scratch()), &self.env, None, 60).unwrap()
        }

        fn run_unconfined(&self, code: &str) -> crate::process_policy::Captured {
            let command = vec![
                self.interpreter.executable.display().to_string(),
                "-I".to_string(),
                "-c".to_string(),
                code.to_string(),
            ];
            run_capture(&command, Some(&self.scratch()), &self.env, None, 60).unwrap()
        }
    }

    fn boundary_is_usable() -> bool {
        match require_working_boundary() {
            Ok(_) => true,
            Err(error) => {
                if cfg!(target_os = "macos") {
                    panic!("sandbox-exec must work on macOS: {}", error.detail);
                }
                eprintln!("skipping: {}", error.detail);
                false
            }
        }
    }

    #[test]
    fn a_confined_child_cannot_read_publisher_files_or_write_outside_scratch() {
        if !boundary_is_usable() {
            return;
        }
        let probe = probe();
        let publisher = tempfile::tempdir().unwrap();
        let sentinel = publisher.path().join("sentinel");
        std::fs::write(&sentinel, "publisher secret").unwrap();
        let escape = publisher.path().join("escaped");
        let code = format!(
            "def attempt(action, path):\n    try:\n        if action == 'read':\n            open(path).read()\n        else:\n            open(path, 'w').write('x')\n        print('BREACH', action)\n    except OSError as error:\n        print('blocked', action, type(error).__name__)\nattempt('read', {sentinel:?})\nattempt('write', {escape:?})\nopen('inside', 'w').write('ok')\nprint('inside ok')\n",
            sentinel = sentinel.to_str().unwrap(),
            escape = escape.to_str().unwrap(),
        );

        let control = probe.run_unconfined(&code);
        assert!(control.stdout.contains("BREACH read") && control.stdout.contains("BREACH write"), "{control:?}");
        std::fs::remove_file(&escape).unwrap();

        let confined = probe.run(Network::Offline, &code);

        assert!(confined.success(), "{confined:?}");
        assert!(confined.stdout.contains("blocked read"), "{confined:?}");
        assert!(confined.stdout.contains("blocked write"), "{confined:?}");
        assert!(!confined.stdout.contains("BREACH"), "{confined:?}");
        assert!(confined.stdout.contains("inside ok"), "{confined:?}");
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "publisher secret");
        assert!(!escape.exists());
    }

    #[test]
    fn offline_children_have_no_network_and_online_children_do() {
        if !boundary_is_usable() {
            return;
        }
        let probe = probe();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let code = format!(
            "import socket\ntry:\n    socket.create_connection(('127.0.0.1', {port}), timeout=5).close()\n    print('connected')\nexcept OSError as error:\n    print('refused', type(error).__name__)\n"
        );
        assert!(probe.run_unconfined(&code).stdout.contains("connected"));
        let offline = probe.run(Network::Offline, &code);
        assert!(offline.stdout.contains("refused"), "{offline:?}");
        let online = probe.run(Network::Online, &code);
        assert!(online.stdout.contains("connected"), "{online:?}");
    }

    #[test]
    fn publisher_credentials_in_the_environment_never_reach_the_child() {
        if !boundary_is_usable() {
            return;
        }
        let mut probe = probe();
        let mut publisher: FxHashMap<String, String> = std::env::vars().collect();
        for name in ["R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY", "R2_S3_SESSION_TOKEN", "CLOUDFLARE_API_TOKEN"] {
            publisher.insert(name.to_string(), "publisher-secret".to_string());
        }
        probe.env = scratch_environment(Some(&publisher), &probe.scratch());
        let result = probe.run(
            Network::Offline,
            "import os\nprint(sorted(k for k in os.environ if k.startswith(('R2_', 'AWS_', 'CLOUDFLARE'))))\n",
        );
        assert!(result.success(), "{result:?}");
        assert_eq!(result.stdout.trim(), "[]");
    }

    #[test]
    fn a_program_reached_through_links_outside_the_grants_still_runs() {
        if !boundary_is_usable() {
            return;
        }
        let probe = probe();
        let links = tempfile::tempdir().unwrap();
        let links = links.path().canonicalize().unwrap();
        let toolchain = links.join("toolchain");
        std::fs::create_dir(&toolchain).unwrap();
        std::os::unix::fs::symlink(&probe.interpreter.executable, toolchain.join("python")).unwrap();
        let alias = links.join("alias");
        std::os::unix::fs::symlink(&toolchain, &alias).unwrap();
        let venv_python = probe.scratch().join("python");
        std::os::unix::fs::symlink(alias.join("python"), &venv_python).unwrap();
        let boundary = Boundary {
            writable: vec![probe.scratch()],
            readable: probe.interpreter.roots.clone(),
            network: Network::Offline,
        };
        let command = vec![venv_python.display().to_string(), "-I".to_string(), "-c".to_string(), "print('ran')".to_string()];

        let confined = confine(&command, &probe.env, &boundary, Stage::Validation).unwrap();
        let result = run_capture(&confined, Some(&probe.scratch()), &probe.env, None, 60).unwrap();

        assert!(result.success(), "{result:?}");
        assert_eq!(result.stdout.trim(), "ran");
    }

    #[test]
    fn a_missing_program_is_reported_instead_of_run_unconfined() {
        let env: FxHashMap<String, String> = [("PATH".to_string(), "/nonexistent".to_string())].into_iter().collect();
        let error = confine(
            &["definitely-not-a-program".to_string()],
            &env,
            &boundary(Path::new("/tmp"), Network::Offline),
            Stage::Build,
        )
        .unwrap_err();
        assert!(error.detail.contains("was not found"), "{}", error.detail);
        assert_eq!(error.exit_code(), 1);
    }
}
