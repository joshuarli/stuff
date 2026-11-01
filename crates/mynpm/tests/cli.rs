#![forbid(unsafe_code)]

use mynpm::config::parse_config;
use mynpm::inventory::Manager;
use std::path::Path;
use std::process::{Command, Output};

const LOCK_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/locks/package-lock-add.json");
const CREDENTIAL_LOCK_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mynpm/locks/package-lock-v3.json");

fn mynpm(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mynpm"))
        .args(args)
        .current_dir(cwd)
        .env_remove("HOME")
        .output()
        .unwrap()
}

fn succeeds(cwd: &Path, args: &[&str]) -> String {
    let out = mynpm(cwd, args);
    assert!(
        out.status.success(),
        "mynpm {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn fails_with(cwd: &Path, args: &[&str], code: i32) -> String {
    let out = mynpm(cwd, args);
    assert_eq!(out.status.code(), Some(code), "mynpm {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn init_config_import_lock_and_plan_through_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();

    let pins = succeeds(cwd, &["pins"]);
    assert_eq!(
        pins,
        format!(
            "npm {}\npnpm {}\n",
            Manager::Npm.pinned_version(),
            Manager::Pnpm.pinned_version()
        )
    );

    succeeds(cwd, &["init-config", "--node-version", "26.12.1", "--all-targets"]);
    let config = parse_config(&std::fs::read_to_string(cwd.join("mynpm.json")).unwrap()).unwrap();
    let ids: Vec<&str> = config.targets.iter().map(|t| t.id()).collect();
    assert_eq!(
        ids,
        ["mac-arm64", "mac-x64", "linux-x64-gnu", "linux-x64-musl", "linux-arm64-gnu", "linux-arm64-musl"]
    );

    let project = cwd.join("web");
    std::fs::create_dir(&project).unwrap();
    std::fs::copy(LOCK_FIXTURE, project.join("package-lock.json")).unwrap();
    let imported = succeeds(
        cwd,
        &["import-lock", "web/package-lock.json", "--project", "web", "--target", "mac-arm64"],
    );
    assert!(imported.contains("for web (mac-arm64)"), "{imported}");

    std::fs::copy(CREDENTIAL_LOCK_FIXTURE, project.join("credential-lock.json")).unwrap();
    let rejected = fails_with(
        cwd,
        &["import-lock", "web/credential-lock.json", "--project", "other", "--target", "mac-arm64"],
        2,
    );
    assert!(rejected.contains("credential-bearing URL"), "{rejected}");

    let plan = succeeds(cwd, &["plan"]);
    assert!(plan.contains("Imported projects: 1"), "{plan}");
    assert!(plan.contains("Observed package versions: 2"), "{plan}");

    let json = succeeds(cwd, &["plan", "--json"]);
    assert!(json.contains("\"projectCount\": 1"), "{json}");
    assert!(json.contains("\"name\": \"transitive\""), "{json}");
    assert!(json.contains("\"targets\": [\n        \"mac-arm64\"\n      ]"), "{json}");
}

#[test]
fn usage_and_input_errors_exit_with_the_shared_codes() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path();

    for (args, message) in [
        (&["fetch", "--json"][..], "--json is only valid with plan"),
        (&["fetch", "--all-targets"], "--all-targets is only valid with init-config"),
        (&["validate"], "validate requires --client"),
        (&["import-lock", "p.json"], "import-lock requires --project"),
        (&["pins", "extra"], "pins does not take positional arguments"),
        (&["pins", "--config", "mynpm.json"], "--config is not valid"),
        (&["nope"], "unknown command: nope"),
        (&["plan", "--bogus"], "unknown option: --bogus"),
        (&[], "a command is required"),
    ] {
        let stderr = fails_with(cwd, args, 2);
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }

    let missing = fails_with(cwd, &["plan"], 2);
    assert!(missing.contains("configuration file does not exist"), "{missing}");

    for args in [&["validate", "--client", "npm"][..], &["add", "--manager", "npm", "a@1.0.0"]] {
        let stderr = fails_with(cwd, args, 2);
        assert!(stderr.contains("configuration file does not exist"), "{args:?}: {stderr}");
    }

    let bad_node = fails_with(cwd, &["init-config", "--node-version", "26.12"], 2);
    assert!(bad_node.contains("major.minor.patch"), "{bad_node}");
}
