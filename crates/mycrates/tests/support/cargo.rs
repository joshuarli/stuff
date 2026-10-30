#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct CargoEnv {
    pub home: PathBuf,
    pub index_url: String,
    pub token: Option<String>,
}

impl CargoEnv {
    pub fn new(home: PathBuf, index_url: String, token: Option<String>) -> Self {
        fs::create_dir_all(&home).unwrap();
        Self {
            home,
            index_url,
            token,
        }
    }

    pub fn cargo_cmd(&self, cwd: &Path) -> Command {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(cwd);
        cmd.env("CARGO_HOME", &self.home);
        cmd.env(
            "CARGO_REGISTRIES_MYCRATES_INDEX",
            format!("sparse+{}", self.index_url),
        );
        if let Some(ref t) = self.token {
            cmd.env("CARGO_REGISTRIES_MYCRATES_TOKEN", t);
        } else {
            cmd.env_remove("CARGO_REGISTRIES_MYCRATES_TOKEN");
        }
        cmd.env("CARGO_TERM_COLOR", "never");
        // Deny non-loopback network egress
        cmd.env("CARGO_HTTP_PROXY", "http://127.0.0.1:1");
        cmd.env("NO_PROXY", "127.0.0.1,localhost");
        cmd
    }

    pub fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut cmd = self.cargo_cmd(cwd);
        for a in args {
            cmd.arg(a);
        }
        cmd.output().expect("failed to execute cargo command")
    }
}

pub fn make_crate_project(
    dir: &Path,
    name: &str,
    version: &str,
    cargo_toml_extra: &str,
    code: &str,
) {
    fs::create_dir_all(dir.join("src")).unwrap();
    let cargo_toml = format!(
        "[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\npublish = [\"mycrates\"]\n\n{}\n",
        name, version, cargo_toml_extra
    );
    fs::write(dir.join("Cargo.toml"), cargo_toml).unwrap();
    fs::write(dir.join("src/lib.rs"), code).unwrap();
}
