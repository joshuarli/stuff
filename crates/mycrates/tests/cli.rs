use mycrates::pins::Pins;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

const CRATES_IO_INDEX: &str = "registry+https://github.com/rust-lang/crates.io-index";
const CRATES_IO_SPARSE: &str = "registry+https://index.crates.io/";
const CHECKSUM: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn mycrates(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mycrates")).args(args).output().unwrap()
}

fn import_lock(lock_dir: &Path, state_dir: &Path, lock: &str) -> Output {
    let lock_path = lock_dir.join("Cargo.lock");
    fs::write(&lock_path, lock).unwrap();
    mycrates(&[
        "import-lock",
        lock_path.to_str().unwrap(),
        "--project",
        "locked",
        "--state-dir",
        state_dir.to_str().unwrap(),
    ])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn import_lock_pins_only_crates_io_packages_and_reports_every_other_source() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let lock = format!(
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "legacy-index"
version = "1.0.0"
source = "{CRATES_IO_INDEX}"
checksum = "{CHECKSUM}"

[[package]]
name = "sparse-index"
version = "2.0.0"
source = "{CRATES_IO_SPARSE}"
checksum = "{CHECKSUM}"

[[package]]
name = "from-git"
version = "0.3.0"
source = "git+https://example.com/from-git.git#abc123"

[[package]]
name = "from-alternate"
version = "0.4.0"
source = "registry+https://alternate.example/index"
checksum = "{CHECKSUM}"

[[package]]
name = "from-path"
version = "0.5.0"
source = "path+file:///work/from-path"
"#
    );

    let output = import_lock(dir.path(), &state_dir, &lock);

    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("imported 2 registry packages for project 'locked'"), "{out}");
    assert!(out.contains("warning: 3 unsupported"), "{out}");
    assert!(out.contains("git: from-git 0.3.0"), "{out}");
    assert!(out.contains("alternate-registry (registry+https://alternate.example/index): from-alternate 0.4.0"), "{out}");
    assert!(out.contains("path: from-path 0.5.0"), "{out}");

    let pins_text = fs::read_to_string(state_dir.join("pins/locked.toml")).unwrap();
    let pins: Pins = mycrates::toml::from_str(&pins_text).unwrap();
    let pinned: Vec<(&str, &str)> = pins.registry.iter().map(|p| (p.name.as_str(), p.source.as_str())).collect();
    assert_eq!(pinned, [("legacy-index", CRATES_IO_INDEX), ("sparse-index", CRATES_IO_SPARSE)]);
    assert_eq!(pins.unsupported.len(), 3, "{pins_text}");
}

#[test]
fn import_lock_fails_closed_and_writes_no_pins() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");

    let missing_checksum = format!(
        "version = 3\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"{CRATES_IO_INDEX}\"\n"
    );
    let output = import_lock(dir.path(), &state_dir, &missing_checksum);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stderr(&output).contains("serde 1.0.0 missing checksum"), "{}", stderr(&output));
    assert!(!state_dir.join("pins/locked.toml").exists());

    let future_version = "version = 99\n";
    let output = import_lock(dir.path(), &state_dir, future_version);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("unknown lock version 99"), "{}", stderr(&output));
    assert!(!state_dir.join("pins/locked.toml").exists());

    let output = import_lock(dir.path(), &state_dir, "version = [");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!state_dir.join("pins/locked.toml").exists());
}

#[test]
fn malformed_command_lines_exit_with_the_input_status() {
    let cases: [(&[&str], &str); 3] = [
        (&["frobnicate"], "unknown command 'frobnicate'"),
        (&["--bind", "127.0.0.1:0"], "expected a command"),
        (&[], "missing command"),
    ];
    for (args, message) in cases {
        let output = mycrates(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", stderr(&output));
        assert!(stderr(&output).contains(message), "{args:?}: {}", stderr(&output));
    }
}
