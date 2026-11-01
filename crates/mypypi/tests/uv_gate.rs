#![forbid(unsafe_code)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

#[test]
fn uv_commands_reject_an_unpinned_uv_before_loading_inputs() {
    let dir = std::env::temp_dir().join(format!("mypypi-uv-gate-{}", std::process::id()));
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let uv = bin.join("uv");
    std::fs::write(&uv, "#!/bin/sh\necho 'uv 0.0.0'\n").unwrap();
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let missing = dir.join("missing");
    for command in [&["add", "six==1.16.0"][..], &["build"][..], &["validate"][..]] {
        let mut args = vec![
            "--catalog".to_string(),
            missing.join("catalog.toml").display().to_string(),
            "--config".to_string(),
            missing.join("pyproject.toml").display().to_string(),
            "--state-dir".to_string(),
            missing.join("state").display().to_string(),
        ];
        args.extend(command.iter().map(|arg| arg.to_string()));
        let output = Command::new(env!("CARGO_BIN_EXE_mypypi"))
            .env("PATH", &path)
            .args(&args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{command:?} accepted uv 0.0.0");
        assert!(
            stderr.contains(&format!("uv {} is required", mypypi::build::UV_VERSION)),
            "{command:?} failed for another reason: {stderr}"
        );
        assert!(
            !missing.join("state").exists(),
            "{command:?} touched state before checking uv"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
