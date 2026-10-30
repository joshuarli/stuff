use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;

struct TestServer {
    child: Child,
    port: u16,
}

impl TestServer {
    fn boot(route_arg: &str) -> Self {
        let bin = env!("CARGO_BIN_EXE_stuff-server");
        let mut child = Command::new(bin)
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--route")
            .arg(route_arg)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn stuff-server");
        let stdout = child.stdout.take().expect("server stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("server banner");
        let addr = line
            .trim()
            .strip_prefix("listening on ")
            .expect("banner format")
            .to_string();
        drop(reader);
        let port: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
        Self { child, port }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn seed_fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/mynpm/test/fixtures/protocol-seed")
}

#[test]
fn npm_installs_from_stuff_server() {
    let seed = seed_fixtures();
    assert!(seed.join("build.mjs").exists(), "protocol seed missing");

    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("site");
    let mirror = site.join("mirror");
    fs::create_dir_all(&mirror).unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&project).unwrap();

    let server = TestServer::boot(&format!("/mirror={}/mirror", site.display()));
    let base = format!("http://127.0.0.1:{}/mirror/", server.port);

    let build = Command::new("node")
        .arg(seed.join("build.mjs"))
        .arg(&site)
        .arg(&base)
        .output()
        .expect("run protocol-seed build.mjs");
    assert!(
        build.status.success(),
        "build.mjs failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let npm_env = |cache: &std::path::Path| {
        vec![
            ("HOME".to_string(), home.to_string_lossy().to_string()),
            (
                "npm_config_userconfig".to_string(),
                home.join("user.npmrc").to_string_lossy().to_string(),
            ),
            (
                "npm_config_globalconfig".to_string(),
                home.join("global.npmrc").to_string_lossy().to_string(),
            ),
            (
                "npm_config_cache".to_string(),
                cache.to_string_lossy().to_string(),
            ),
            ("npm_config_audit".to_string(), "false".to_string()),
            ("npm_config_fund".to_string(), "false".to_string()),
        ]
    };
    fs::write(home.join("user.npmrc"), "").unwrap();
    fs::write(home.join("global.npmrc"), "").unwrap();
    fs::write(project.join("package.json"), "{\"name\":\"consumer\",\"version\":\"1.0.0\",\"private\":true}\n").unwrap();

    let cache1 = tmp.path().join("cache1");
    let install = Command::new("npm")
        .current_dir(&project)
        .arg("install")
        .arg("@flatrepo/demo@2.3.4")
        .arg("--registry")
        .arg(format!("{base}current/"))
        .arg("--ignore-scripts")
        .arg("--no-audit")
        .arg("--no-fund")
        .arg("--fetch-retries=0")
        .envs(npm_env(&cache1))
        .output()
        .expect("run npm install");
    assert!(
        install.status.success(),
        "npm install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let check = Command::new("node")
        .arg("-e")
        .arg("const assert = require(\"assert/strict\"); assert.equal(require(\"@flatrepo/demo\"), \"static registry works (via @flatrepo/demo)\"); console.log(\"PASS scoped + transitive dependency (npm install)\");")
        .current_dir(&project)
        .output()
        .expect("run node check");
    assert!(check.status.success(), "scoped require failed: {}", String::from_utf8_lossy(&check.stderr));

    let lock = fs::read_to_string(project.join("package-lock.json")).unwrap();
    assert!(lock.contains(&format!("127.0.0.1:{}/mirror/current/-/tarballs/", server.port)));
    assert!(!lock.contains("registry.npmjs.org"));

    fs::remove_dir_all(project.join("node_modules")).unwrap();
    let cache2 = tmp.path().join("cache2");
    let ci = Command::new("npm")
        .current_dir(&project)
        .arg("ci")
        .arg("--registry")
        .arg(format!("{base}current/"))
        .arg("--ignore-scripts")
        .arg("--no-audit")
        .arg("--no-fund")
        .arg("--fetch-retries=0")
        .envs(npm_env(&cache2))
        .output()
        .expect("run npm ci");
    assert!(
        ci.status.success(),
        "npm ci failed: {}",
        String::from_utf8_lossy(&ci.stderr)
    );
    let check = Command::new("node")
        .arg("-e")
        .arg("const assert = require(\"assert/strict\"); assert.equal(require(\"@flatrepo/demo\"), \"static registry works (via @flatrepo/demo)\"); console.log(\"PASS fresh cache + lockfile (npm ci)\");")
        .current_dir(&project)
        .output()
        .expect("run node check");
    assert!(check.status.success(), "scoped require after ci failed");
}
