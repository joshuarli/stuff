#![forbid(unsafe_code)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE_WHEEL: &str = "validation_fixture-1.0-py3-none-any.whl";

const FAKE_PYTHON: &str = r#"#!/bin/sh
case "$*" in
*cache_tag*) printf 'cpython\n3.15.1\ncpython-315\nfake-platform\n' ;;
*sys.executable*) echo /fake/python ;;
*version_info.micro*) echo '3 15 1' ;;
*implementation.name*) echo 'cpython 3 15' ;;
esac
"#;

struct World {
    dir: tempfile::TempDir,
    platform: &'static str,
}

impl World {
    fn new() -> Self {
        let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "macos-arm64",
            ("macos", "x86_64") => "macos-x86_64",
            ("linux", "x86_64") => "linux-x86_64",
            ("linux", "aarch64") => "linux-aarch64",
            other => panic!("unsupported host {other:?}"),
        };
        let world = World { dir: tempfile::tempdir().unwrap(), platform };
        let bin = world.path("bin");
        std::fs::create_dir(&bin).unwrap();
        world.script("bin/python", FAKE_PYTHON);
        world.script(
            "bin/uv",
            &format!(
                r#"#!/bin/sh
echo "$*" >> "{log}"
case "$1" in
--version) echo "uv {version} (fake)" ;;
venv)
  for last; do :; done
  mkdir -p "$last/bin"
  printf '#!/bin/sh\nexit 0\n' > "$last/bin/python"
  chmod +x "$last/bin/python"
  ;;
esac
"#,
                log = world.path("uv.log").display(),
                version = mypypi::build::UV_VERSION,
            ),
        );
        std::fs::write(
            world.path("pyproject.toml"),
            format!(
                "[project]\nrequires-python = \">=3.15,<3.16\"\n\n[tool.mypypi]\npublic_base_url = \"https://pypi.example.test\"\n\n[tool.mypypi.targets]\nplatforms = [\"{platform}\"]\nlinux_repair_policy = \"manylinux_2_28\"\n"
            ),
        )
        .unwrap();
        world.set_catalog(&["validation-fixture"]);
        std::fs::create_dir_all(world.path("state/wheels")).unwrap();
        world
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn script(&self, name: &str, body: &str) {
        let path = self.path(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn set_catalog(&self, packages: &[&str]) {
        let text: String = packages
            .iter()
            .map(|name| format!("[[packages]]\nname = \"{name}\"\nversion = \"1.0\"\n\n"))
            .collect();
        std::fs::write(self.path("packages.toml"), text).unwrap();
    }

    fn stage_fixture_wheel(&self) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mypypi/wheels").join(FIXTURE_WHEEL);
        let staged = self.path("state/wheels").join(FIXTURE_WHEEL);
        std::fs::copy(source, &staged).unwrap();
        staged
    }

    fn mypypi(&self, args: &[&str]) -> Output {
        let path = format!("{}:{}", self.path("bin").display(), std::env::var("PATH").unwrap_or_default());
        Command::new(env!("CARGO_BIN_EXE_mypypi"))
            .env("PATH", path)
            .env("PYTHON", self.path("bin/python"))
            .args(["--catalog"])
            .arg(self.path("packages.toml"))
            .arg("--config")
            .arg(self.path("pyproject.toml"))
            .arg("--state-dir")
            .arg(self.path("state"))
            .args(args)
            .output()
            .unwrap()
    }

    fn output_dir_args(&self, command: &str) -> Vec<String> {
        vec![command.to_string(), "--output".to_string(), self.path("out").display().to_string()]
    }

    fn run_with_output(&self, command: &str, extra: &[&str]) -> Output {
        let mut args = self.output_dir_args(command);
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.mypypi(&refs)
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn succeeds(output: Output) -> String {
    assert!(output.status.success(), "exit {:?}\nstdout: {}\nstderr: {}", output.status.code(), stdout(&output), stderr(&output));
    stdout(&output)
}

fn fails_with(output: Output, code: i32, needle: &str) {
    assert_eq!(output.status.code(), Some(code), "stdout: {}\nstderr: {}", stdout(&output), stderr(&output));
    assert!(stderr(&output).contains(needle), "stderr lacks {needle:?}: {}", stderr(&output));
}

#[test]
fn staged_wheel_is_planned_validated_published_and_republished() {
    let world = World::new();
    let platform = world.platform;
    let wheel = world.stage_fixture_wheel();

    let planned = succeeds(world.run_with_output("plan", &[]));
    assert!(planned.contains(&format!("validate {FIXTURE_WHEEL} for {platform}")), "{planned}");

    let validated = succeeds(world.mypypi(&["validate", "--output", world.path("out").to_str().unwrap()]));
    assert!(validated.contains("validated 1 new wheel(s)"), "{validated}");
    let uv_log = std::fs::read_to_string(world.path("uv.log")).unwrap();
    assert!(uv_log.contains("pip install") && uv_log.contains("--no-index") && uv_log.contains(FIXTURE_WHEEL), "{uv_log}");
    assert!(!world.path("out").exists(), "validate must not publish");

    let published = succeeds(world.run_with_output("publish", &[]));
    assert!(published.contains("published 1 new wheel(s)"), "{published}");
    let current = world.path("out/current");
    assert_eq!(std::fs::read(world.path("out/wheels").join(FIXTURE_WHEEL)).unwrap(), std::fs::read(&wheel).unwrap());
    let project_page = std::fs::read_to_string(current.join("simple/validation-fixture/index.html")).unwrap();
    assert!(project_page.contains(FIXTURE_WHEEL) && project_page.contains("sha256="), "{project_page}");
    assert!(std::fs::read_to_string(current.join("llms.txt")).unwrap().contains("https://pypi.example.test"));

    let replanned = succeeds(world.run_with_output("plan", &[]));
    assert!(replanned.contains(&format!("published validation-fixture==1.0 for {platform}")), "{replanned}");

    let again = succeeds(world.run_with_output("publish", &[]));
    assert!(again.contains("published 0 new wheel(s)"), "{again}");

    let before = std::fs::read_link(&current).unwrap();
    let republished = succeeds(world.mypypi(&["republish", "--output", world.path("out").to_str().unwrap(), "--public-url", "https://mirror.example.test"]));
    assert!(republished.contains("republished"), "{republished}");
    let after = std::fs::read_link(&current).unwrap();
    assert_ne!(before, after);
    let llms = std::fs::read_to_string(current.join("llms.txt")).unwrap();
    assert!(llms.contains("https://mirror.example.test") && !llms.contains("https://pypi.example.test"), "{llms}");
    assert_eq!(std::fs::read(world.path("out/wheels").join(FIXTURE_WHEEL)).unwrap(), std::fs::read(&wheel).unwrap());
}

#[test]
fn failures_exit_with_the_contract_code_and_name_the_cause() {
    let world = World::new();
    let platform = world.platform;
    let out = world.path("out");
    let out = out.to_str().unwrap();

    fails_with(world.mypypi(&["publish"]), 2, "--output");
    fails_with(world.mypypi(&["plan", "extra"]), 2, "does not take positional arguments");
    fails_with(world.mypypi(&["add"]), 2, "add");
    fails_with(world.mypypi(&["republish", "--output", out]), 2, "--public-url");
    fails_with(world.mypypi(&["republish", "--output", out, "--public-url", "https://mirror.example.test"]), 2, "no published index to republish");
    assert!(!world.path("out").exists());
    fails_with(world.mypypi(&["plan", "--uv-lock", "uv.lock"]), 2, "apply only to add");
    fails_with(world.mypypi(&["plan", "--strict-build-deps"]), 2, "build dependency arguments");
    fails_with(world.mypypi(&["add", "six==1.16.0", "--platform", platform]), 2, "--platform");
    fails_with(world.mypypi(&["plan", "--platform", "plan9-mips"]), 2, "unconfigured platform: plan9-mips");

    fails_with(world.mypypi(&["publish", "--output", out]), 1, &format!("no candidate wheel for {platform}"));
    assert!(!world.path("out").exists());

    let wheel = world.stage_fixture_wheel();
    succeeds(world.mypypi(&["publish", "--output", out]));
    let mut bytes = std::fs::read(&wheel).unwrap();
    bytes.push(0);
    std::fs::write(&wheel, bytes).unwrap();
    fails_with(
        world.mypypi(&["plan", "--output", out]),
        4,
        &format!("mypypi: wheel filename differs from published bytes: {FIXTURE_WHEEL} (validation-fixture==1.0, Python 3.15 on {platform}, stage planning)"),
    );
    fails_with(world.mypypi(&["publish", "--output", out]), 4, "differs from published bytes");

    std::fs::remove_file(&wheel).unwrap();
    world.stage_fixture_wheel();
    world.set_catalog(&["other-package"]);
    fails_with(world.mypypi(&["plan", "--output", out]), 2, "not listed");
}

#[test]
fn excluded_platform_entries_are_skipped_without_fetching() {
    let world = World::new();
    let platform = world.platform;
    std::fs::write(
        world.path("packages.toml"),
        format!(
            "[[packages]]\nname = \"native-only\"\nversion = \"2.0\"\n[packages.exclude_platforms]\n\"{platform}\" = \"no wheel here\"\n"
        ),
    )
    .unwrap();
    let planned = succeeds(world.mypypi(&["plan"]));
    assert!(planned.contains(&format!("skip native-only==2.0 for {platform}: no wheel here")), "{planned}");
}

#[test]
fn uv_commands_reject_an_unpinned_uv_before_loading_inputs() {
    let world = World::new();
    world.set_catalog(&[]);
    std::fs::remove_dir_all(world.path("state")).unwrap();
    world.script("bin/uv", "#!/bin/sh\necho 'uv 0.0.0'\n");
    for command in [&["add", "six==1.16.0"][..], &["build"][..], &["validate"][..]] {
        let output = world.mypypi(command);
        fails_with(output, 1, &format!("uv {} is required", mypypi::build::UV_VERSION));
    }
    assert!(!world.path("state").exists());
}

#[test]
fn bad_project_configuration_is_rejected_before_any_work() {
    let world = World::new();
    world.set_catalog(&[]);
    let config = world.path("pyproject.toml");
    let valid = std::fs::read_to_string(&config).unwrap();
    let cases = [
        (valid.replace(">=3.15,<3.16", ">=3.14,<3.16"), "exactly one Python minor"),
        (valid.replace("linux_repair_policy", "extra = \"x\"\nlinux_repair_policy"), "unknown target setting: extra"),
        (valid.replace(&format!("\"{}\"]", world.platform), &format!("\"{0}\", \"{0}\"]", world.platform)), "must be unique"),
        (valid.replace("https://pypi.example.test", "ftp://pypi.example.test"), "https or http"),
    ];
    for (text, needle) in cases {
        std::fs::write(&config, text).unwrap();
        fails_with(world.mypypi(&["plan"]), 2, needle);
    }
}

#[test]
fn published_wheels_are_not_revalidated() {
    let world = World::new();
    world.stage_fixture_wheel();
    succeeds(world.run_with_output("publish", &[]));
    let calls = std::fs::read_to_string(world.path("uv.log")).unwrap().lines().count();
    succeeds(world.run_with_output("publish", &[]));
    succeeds(world.run_with_output("plan", &[]));
    assert_eq!(std::fs::read_to_string(world.path("uv.log")).unwrap().lines().count(), calls, "republishing known wheels must not revalidate");
}
