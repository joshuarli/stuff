#![forbid(unsafe_code)]

use crate::acquire::{
    CompatibilityCategory, CompatibilityRejection, Release, ReleaseFile, download_artifact,
    select_or_download, validate_wheel_architectures, validate_wheel_metadata_at, sha256_file,
};
use crate::error::{Error, Kind, Result, Stage};
use crate::models::{PackageVersion, Target};
use crate::platforms::{MACOS_DEPLOYMENT_TARGET, native_architectures_from_bytes, target_python_version};
use crate::process_policy::{
    redacted_diagnostic, run_capture, scratch_environment, seed_toolchain_home, toolchain_read_roots,
};
use crate::sandbox::{Boundary, Network, confine, interpreter};
use rustc_hash::{FxHashMap, FxHashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

const BINARY_EXTENSIONS: &[&str] = &[".c", ".cc", ".cpp", ".cxx", ".pxd", ".pxi", ".pyx", ".go", ".rs"];
const BINARY_SUFFIXES: &[&str] = &[".so", ".dylib", ".pyd", ".dll", ".exe"];
const SDIST_EXPANDED_LIMIT: u64 = 1024 * 1024 * 1024;
const HEAD_BYTES: usize = 1024 * 1024;

pub const UV_VERSION: &str = include_str!("../../../deploy/uv-version").trim_ascii();

pub const BUILD_CONSTRAINTS: &str = include_str!("../../../deploy/build-constraints.txt");

const LEGACY_BUILD_REQUIRES: [&str; 2] = ["setuptools>=40.8.0", "wheel"];
const SDIST_PYPROJECT_LIMIT: usize = 1024 * 1024;

fn uv_version_in(output: &str) -> Option<&str> {
    output.split_whitespace().nth(1).filter(|version| !version.is_empty())
}

pub(crate) fn require_uv_version() -> Result<()> {
    let output = std::process::Command::new("uv")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            Error::dependency(
                format!("uv {UV_VERSION} is required: {e}"),
                Stage::Command,
            )
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found = uv_version_in(&text);
    if !output.status.success() || found != Some(UV_VERSION) {
        return Err(Error::dependency(
            format!(
                "uv {UV_VERSION} is required; found {}",
                found.unwrap_or("no version output")
            ),
            Stage::Command,
        ));
    }
    Ok(())
}

fn is_sdist_pyproject(name: &str) -> bool {
    let stripped = name.strip_suffix('/').unwrap_or(name);
    let mut parts = stripped.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(dir), Some("pyproject.toml"), None) if !dir.is_empty()
    )
}

fn read_sdist_pyproject(source_path: &Path) -> Result<Option<Vec<u8>>> {
    if let Ok(archive) = crate::wheel_zip::Archive::open(source_path) {
        for name in archive.names() {
            if !is_sdist_pyproject(name) {
                continue;
            }
            let bytes = archive
                .read_prefix(name, SDIST_PYPROJECT_LIMIT as u64 + 1)
                .map_err(|e| Error::build(format!("cannot inspect source archive pyproject.toml: {e}"), Stage::Build))?;
            if bytes.len() > SDIST_PYPROJECT_LIMIT {
                return Err(Error::build("sdist pyproject.toml exceeds 1 MiB", Stage::Build));
            }
            return Ok(Some(bytes));
        }
        return Ok(None);
    }
    let file = std::fs::File::open(source_path).map_err(|e| {
        Error::build(format!("cannot inspect source archive {source_path}: {e}", source_path = source_path.display()), Stage::Build)
    })?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|e| Error::build(format!("cannot inspect source archive {source_path}: {e}", source_path = source_path.display()), Stage::Build))?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::build(format!("cannot inspect source archive {source_path}: {e}", source_path = source_path.display()), Stage::Build)
        })?;
        let name = std::str::from_utf8(&entry.path_bytes())
            .map_err(|_| Error::build("cannot inspect source archive: sdist member is not valid UTF-8", Stage::Build))?
            .to_string();
        if !is_sdist_pyproject(&name) {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .take(SDIST_PYPROJECT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::build(format!("cannot inspect source archive pyproject.toml: {e}"), Stage::Build))?;
        if bytes.len() > SDIST_PYPROJECT_LIMIT {
            return Err(Error::build("sdist pyproject.toml exceeds 1 MiB", Stage::Build));
        }
        return Ok(Some(bytes));
    }
    Ok(None)
}

/// The build requirements an sdist declares in its `pyproject.toml`, or the
/// legacy setuptools fallback when the sdist has no `build-system` table.
pub fn sdist_build_requires(source_path: &Path) -> Result<Vec<String>> {
    let legacy = || LEGACY_BUILD_REQUIRES.iter().map(|value| value.to_string()).collect::<Vec<_>>();
    let Some(bytes) = read_sdist_pyproject(source_path)? else {
        return Ok(legacy());
    };
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| Error::build("sdist pyproject.toml is not valid UTF-8", Stage::Build))?;
    let value: deser_value::Value = deser_toml::from_str(source)
        .map_err(|e| Error::build(format!("sdist pyproject.toml is not valid TOML: {e}"), Stage::Build))?;
    let Some(build_system) = value.as_map().and_then(|doc| doc.get("build-system")) else {
        return Ok(legacy());
    };
    let Some(requires) = build_system.as_map().and_then(|table| table.get("requires")) else {
        return Ok(legacy());
    };
    let entries = requires
        .as_seq()
        .ok_or_else(|| Error::build("sdist pyproject.toml build-system.requires must be an array of strings", Stage::Build))?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let requirement = entry
            .as_str()
            .ok_or_else(|| Error::build("sdist pyproject.toml build-system.requires must be an array of strings", Stage::Build))?
            .trim();
        if requirement.is_empty() {
            return Err(Error::build("sdist pyproject.toml build-system.requires contains an empty requirement", Stage::Build));
        }
        if requirement.contains('\n') || requirement.contains('\r') {
            return Err(Error::build("sdist pyproject.toml build-system.requires contains a requirement spanning multiple lines", Stage::Build));
        }
        if !out.iter().any(|existing| existing == requirement) {
            out.push(requirement.to_string());
        }
    }
    Ok(out)
}

pub fn validate_build_index(url: &str, strict: bool) -> Result<String> {
    let parsed: stuff_url::Url = url.parse().map_err(|_| {
        Error::configuration(
            "build dependency index must use credential-free HTTPS, loopback HTTP, or an absolute file URL",
        )
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::configuration(
            "build dependency index must use credential-free HTTPS, loopback HTTP, or an absolute file URL",
        ));
    }
    if !parsed.query().unwrap_or("").is_empty() || parsed.fragment().is_some() {
        return Err(Error::configuration(
            "build dependency index must use credential-free HTTPS, loopback HTTP, or an absolute file URL",
        ));
    }
    let host = parsed.host_str().unwrap_or("").trim_end_matches('.').to_lowercase();
    let is_loopback_host = host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    let remote_https = parsed.scheme() == "https" && parsed.host_str().is_some();
    let local_http = parsed.scheme() == "http" && is_loopback_host && parsed.host_str().is_some();
    let local_file = parsed.scheme() == "file"
        && (parsed.host_str().is_none() || parsed.host_str() == Some("localhost"))
        && Path::new(parsed.path()).is_absolute();
    if !(remote_https || local_http || local_file) {
        return Err(Error::configuration(
            "build dependency index must use credential-free HTTPS, loopback HTTP, or an absolute file URL",
        ));
    }
    if strict && remote_https {
        let public = ["pypi.org", "pythonhosted.org"];
        if public.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) {
            return Err(Error::configuration(
                "--strict-build-deps requires a curated build dependency index",
            ));
        }
    }
    Ok(url.to_string())
}

pub fn require_native_target(target: &Target) -> Result<()> {
    let host = crate::platforms::current_platform();
    if host.as_deref() != Some(target.platform.as_str()) {
        return Err(Error::incompatible(
            format!(
                "native builds require a matching host; host is {}, target is {}",
                host.as_deref().unwrap_or("unsupported"),
                target.platform
            ),
            Stage::Build,
        )
        .with_target(&target.python_version.to_string(), &target.platform));
    }
    Ok(())
}

fn safe_member(name: &str) -> bool {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return false;
    }
    if name.starts_with('/') {
        return false;
    }
    let stripped = name.strip_suffix('/').unwrap_or(name);
    let parts: Vec<&str> = stripped.split('/').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return false;
    }
    if parts[0].contains(':') {
        return false;
    }
    true
}

pub fn likely_binary(sdist: &Path, ignored_paths: &[String]) -> Result<Option<String>> {
    let ignored: rustc_hash::FxHashSet<&str> = ignored_paths.iter().map(|s| s.as_str()).collect();
    // zip sdist?
    std::fs::File::open(sdist).map_err(|e| {
        Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
    })?;
    let zip_archive = crate::wheel_zip::Archive::open(sdist).ok();
    let mut extensions: FxHashMap<String, bool> = FxHashMap::default();
    let mut native_payloads: FxHashMap<String, bool> = FxHashMap::default();
    let mut setup_py: Vec<u8> = Vec::new();
    if let Some(archive) = zip_archive {
        let names: Vec<String> = archive.names().map(str::to_string).collect();
        let res = {
            let mut seen = rustc_hash::FxHashSet::default();
            names.iter().any(|n| !seen.insert(n.clone()))
        }; if res {
            return Err(Error::build(
                format!("cannot inspect source archive {sdist}: source archive contains duplicate paths", sdist = sdist.display()),
                Stage::Build,
            ));
        }
        for name in &names {
            if !safe_member(name) {
                return Err(Error::build(format!("cannot inspect source archive: unsafe sdist member: {name}"), Stage::Build));
            }
        }
        for name in &names {
            if name.ends_with('/') {
                continue;
            }
            if name.contains("/test/") || name.contains("/tests/") || ignored.contains(name.as_str()) {
                continue;
            }
            if name.ends_with("/setup.py") {
                let buf =
                    archive.read(name).map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?;
                setup_py.extend_from_slice(&buf);
            }
            if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str()) {
                let dot = format!(".{}", ext.to_lowercase());
                if BINARY_EXTENSIONS.contains(&dot.as_str()) {
                    extensions.insert(dot, true);
                }
            }
            let content = archive
                .read_prefix(name, 1024 * 1024)
                .map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?;
            if !native_architectures_from_bytes(&content).map_err(|e| {
                Error::integrity(format!("malformed native header in {name}: {e}"), Stage::Build)
            })?.is_empty()
            {
                native_payloads.insert(name.clone(), true);
            }
        }
    } else {
        let file = std::fs::File::open(sdist).map_err(|e| {
            Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
        })?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        let entries = archive.entries().map_err(|e| {
            Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
        })?;
        let mut seen: FxHashSet<String> = FxHashSet::default();
        let mut expanded: u64 = 0;
        for entry in entries {
            let entry = entry.map_err(|e| {
                Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
            })?;
            let name = std::str::from_utf8(&entry.path_bytes())
                .map_err(|_| Error::build("cannot inspect source archive: sdist member is not valid UTF-8", Stage::Build))?
                .to_string();
            if !safe_member(&name) {
                return Err(Error::build(format!("cannot inspect source archive: unsafe sdist member: {name}"), Stage::Build));
            }
            let member = name.strip_suffix('/').unwrap_or(&name).to_string();
            if !seen.insert(member.clone()) {
                return Err(Error::build("cannot inspect source archive: source archive contains duplicate paths", Stage::Build));
            }
            expanded = expanded.checked_add(entry.size()).ok_or_else(|| {
                Error::build(format!("cannot inspect source archive: expanded size exceeds {SDIST_EXPANDED_LIMIT} bytes"), Stage::Build)
            })?;
            if expanded > SDIST_EXPANDED_LIMIT {
                return Err(Error::build(format!("cannot inspect source archive: expanded size exceeds {SDIST_EXPANDED_LIMIT} bytes"), Stage::Build));
            }
            match entry.header().entry_type() {
                tar::EntryType::Regular => {}
                tar::EntryType::Directory => continue,
                _ => {
                    return Err(Error::build(format!("cannot inspect source archive: unsupported source archive member: {name}"), Stage::Build));
                }
            }
            if member.contains("/test/") || member.contains("/tests/") || ignored.contains(member.as_str()) {
                continue;
            }
            let mut content = Vec::new();
            entry.take(HEAD_BYTES as u64).read_to_end(&mut content).map_err(|e| {
                Error::build(format!("cannot inspect source archive: {e}"), Stage::Build)
            })?;
            if member.ends_with("/setup.py") {
                setup_py.extend_from_slice(&content);
            }
            if let Some(ext) = Path::new(&member).extension().and_then(|e| e.to_str()) {
                let dot = format!(".{}", ext.to_lowercase());
                if BINARY_EXTENSIONS.contains(&dot.as_str()) {
                    extensions.insert(dot, true);
                }
            }
            if !native_architectures_from_bytes(&content).map_err(|e| {
                Error::integrity(format!("malformed native header in {member}: {e}"), Stage::Build)
            })?.is_empty()
            {
                native_payloads.insert(member.clone(), true);
            }
        }
    }
    if !extensions.is_empty() {
        let mut list: Vec<&String> = extensions.keys().collect();
        list.sort_unstable();
        let joined = list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        return Ok(Some(format!("source archive contains compiled-source files: {joined}")));
    }
    if !native_payloads.is_empty() {
        let mut list: Vec<&String> = native_payloads.keys().collect();
        list.sort_unstable();
        let joined = list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        return Ok(Some(format!("source archive contains native payloads: {joined}")));
    }
    if contains_bytes(&setup_py, b"cffi_modules") {
        return Ok(Some("source archive setup.py declares cffi_modules".to_string()));
    }
    Ok(None)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

pub fn produced_binary(wheel: &Path) -> Result<bool> {
    let archive = crate::wheel_zip::Archive::open(wheel).map_err(|e| {
        Error::build(format!("cannot inspect built wheel {wheel}: {e}", wheel = wheel.display()), Stage::Build)
    })?;
    let names: Vec<String> = archive.names().map(str::to_string).collect();
    let res = {
        let mut seen = rustc_hash::FxHashSet::default();
        names.iter().any(|n| !seen.insert(n.clone()))
    }; if res {
        return Err(Error::build("cannot inspect built wheel: wheel contains duplicate archive paths", Stage::Build));
    }
    for name in &names {
        if !safe_member(name) {
            return Err(Error::build(format!("cannot inspect built wheel: unsafe wheel member: {name}"), Stage::Build));
        }
    }
    for name in &names {
        if name.ends_with('/') {
            continue;
        }
        let content = archive
            .read_prefix(name, 1024 * 1024)
            .map_err(|e| Error::build(format!("cannot inspect built wheel: {e}"), Stage::Build))?;
        if BINARY_SUFFIXES.iter().any(|s| name.ends_with(s)) || name.contains(".so.") {
            return Ok(true);
        }
        if !native_architectures_from_bytes(&content).map_err(|e| {
            Error::integrity(format!("malformed native header in built wheel {wheel}: {e}", wheel = wheel.display()), Stage::Build)
        })?.is_empty()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn validate_wheel_identity(wheel: &Path, package: &PackageVersion) -> Result<()> {
    let filename = wheel.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let identity = crate::models::WheelIdentity::parse(filename).map_err(|e| {
        Error::integrity(format!("invalid wheel filename {filename}: {}", e.detail), Stage::Acquisition).with_package(package)
    })?;
    if identity.package != *package {
        return Err(Error::integrity(
            format!("wheel filename {filename} does not match {package}"),
            Stage::Acquisition,
        )
        .with_package(package));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub struct BuildOptions<'a> {
    pub index_url: &'a str,
    pub likely_binary_ignore: &'a [String],
    pub custom_prebuild: &'a [(String, String)],
    pub linux_repair_policy: &'a str,
    pub build_constraints: Option<&'a Path>,
    pub require_build_hashes: bool,
    pub env: Option<&'a FxHashMap<String, String>>,
    pub prebuild_cwd: Option<&'a Path>,
}

pub fn build_from_sdist(
    package: &PackageVersion,
    target: &Target,
    source: &ReleaseFile,
    destination: &Path,
    opts: &BuildOptions<'_>,
) -> Result<PathBuf> {
    require_native_target(target)?;
    validate_build_index(opts.index_url, false)?;
    if let Some(constraints) = opts.build_constraints {
        let resolved = constraints.canonicalize().map_err(|_| {
            Error::configuration(
                format!("could not read build constraints at {}", constraints.display()),
            )
            .with_package(package)
            .with_target(&target.python_version.to_string(), &target.platform)
        })?;
        let md = std::fs::symlink_metadata(&resolved).map_err(|_| {
            Error::configuration(format!("could not read build constraints at {}", constraints.display()))
                .with_package(package)
                .with_target(&target.python_version.to_string(), &target.platform)
        })?;
        if md.file_type().is_symlink() || !md.file_type().is_file() {
            return Err(Error::configuration(format!("build constraints must be a regular file: {}", constraints.display()))
                .with_package(package)
                .with_target(&target.python_version.to_string(), &target.platform));
        }
    }
    let destination = std::path::absolute(destination)
        .map_err(|e| Error::io(format!("could not create build destination: {e}"), Stage::Build).with_package(package).with_target(&target.python_version.to_string(), &target.platform))?;
    std::fs::create_dir_all(&destination)
        .map_err(|e| Error::io(format!("could not create build destination {}: {e}", destination.display()), Stage::Build).with_package(package).with_target(&target.python_version.to_string(), &target.platform))?;
    {
        let md = std::fs::symlink_metadata(&destination).map_err(|e| {
            Error::io(format!("could not create build destination: {e}"), Stage::Build).with_package(package).with_target(&target.python_version.to_string(), &target.platform)
        })?;
        if md.file_type().is_symlink() || !md.file_type().is_dir() {
            return Err(Error::invalid_input(
                format!("build destination must be a real directory: {}", destination.display()),
                Stage::Build,
            )
            .with_package(package)
            .with_target(&target.python_version.to_string(), &target.platform));
        }
    }
    let prebuild_cwd = opts.prebuild_cwd.map(|p| p.to_path_buf()).unwrap_or(std::env::current_dir().map_err(|e| {
        Error::io(format!("could not determine working directory: {e}"), Stage::Build)
    })?);
    let stage = destination.join(format!(".mypypi-build-{:x}", rand_hex()));
    std::fs::create_dir(&stage).map_err(|e| {
        Error::io(format!("could not create build staging directory {stage}: {e}", stage = stage.display()), Stage::Build).with_package(package).with_target(&target.python_version.to_string(), &target.platform)
    })?;
    let started = Instant::now();
    let result: Result<PathBuf> = build_inner(package, target, source, &destination, &stage, &prebuild_cwd, opts);
    let _ = started.elapsed();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    // on success build_inner removes stage? No: it promotes then cleanup; ensure cleanup
    if stage.exists() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
}

fn rand_hex() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (nanos as u64) ^ ((std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15))
}

fn build_environment(
    base: &FxHashMap<String, String>,
    stage: &Path,
) -> std::io::Result<FxHashMap<String, String>> {
    let home = stage.join("home");
    std::fs::create_dir(&home)?;
    seed_toolchain_home(base, &home)?;
    let mut env = scratch_environment(Some(base), &home);
    let cargo_home = home.join(".cargo");
    let rustup_home = home.join(".rustup");
    env.insert("CARGO_HOME".to_string(), cargo_home.display().to_string());
    env.insert("RUSTUP_HOME".to_string(), rustup_home.display().to_string());
    let cargo_bin = cargo_home.join("bin").display().to_string();
    let old_path = env.get("PATH").cloned().unwrap_or_default();
    env.insert(
        "PATH".to_string(),
        if old_path.is_empty() {
            cargo_bin
        } else {
            format!("{cargo_bin}:{old_path}")
        },
    );
    let temp = stage.join("tmp").display().to_string();
    env.insert("ARCHFLAGS".to_string(), String::new());
    env.insert("TMPDIR".to_string(), temp.clone());
    env.insert("TEMP".to_string(), temp.clone());
    env.insert("TMP".to_string(), temp);
    Ok(env)
}

fn prime_checked(
    command: &[String],
    run: &Run<'_>,
    package: &PackageVersion,
    target: &Target,
) -> Result<()> {
    let ctx = |e: Error| e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
    let confined;
    let argv = match run.boundary {
        Some(boundary) => {
            confined = confine(command, run.env, boundary, Stage::Build).map_err(ctx)?;
            &confined[..]
        }
        None => command,
    };
    match run_capture(argv, Some(run.cwd), run.env, None, run.timeout) {
        Ok(out) if out.success() => Ok(()),
        Ok(out) => {
            let mut detail = format!("could not resolve the build requirements for {package}: {}", command.join(" "));
            let stderr = out.stderr.trim();
            if !stderr.is_empty() {
                detail = format!("{detail}: {}", redacted_diagnostic(stderr, 2000));
            }
            Err(ctx(Error::dependency(detail, Stage::Build)))
        }
        Err(crate::process_policy::RunError::Timeout { stderr, .. }) => {
            let mut detail = format!("could not resolve the build requirements for {package}: timed out");
            if !stderr.trim().is_empty() {
                detail = format!("{detail}: {}", redacted_diagnostic(stderr.trim(), 2000));
            }
            Err(ctx(Error::dependency(detail, Stage::Build)))
        }
        Err(e) => Err(ctx(Error::dependency(format!("could not resolve the build requirements for {package}: {e}"), Stage::Build))),
    }
}

#[allow(clippy::too_many_arguments)]
fn prime_build_requirements(
    requires: &[String],
    constraints: &Path,
    python_executable: &Path,
    build_env: &FxHashMap<String, String>,
    readable: &[PathBuf],
    stage: &Path,
    index_url: &str,
    package: &PackageVersion,
    target: &Target,
) -> Result<()> {
    if requires.is_empty() {
        return Ok(());
    }
    let requirements = stage.join("build-requirements.in");
    let mut body = requires.join("\n");
    body.push('\n');
    std::fs::write(&requirements, body).map_err(|e| {
        Error::io(format!("could not stage the build requirements file {}: {e}", requirements.display()), Stage::Build)
            .with_package(package)
            .with_target(&target.python_version.to_string(), &target.platform)
    })?;
    let resolved = stage.join("build-requirements.txt");
    let boundary = Boundary { writable: vec![stage.to_path_buf()], readable: readable.to_vec(), network: Network::Online };
    let compile = vec![
        "uv".to_string(),
        "pip".to_string(),
        "compile".to_string(),
        requirements.display().to_string(),
        "--universal".to_string(),
        format!("--constraints={}", constraints.display()),
        "--generate-hashes".to_string(),
        "--no-config".to_string(),
        format!("--default-index={index_url}"),
        format!("-o={}", resolved.display()),
    ];
    prime_checked(&compile, &Run { cwd: stage, env: build_env, timeout: 600, boundary: Some(&boundary) }, package, target)?;
    let warm = stage.join("build-requirements-target");
    let install = vec![
        "uv".to_string(),
        "pip".to_string(),
        "install".to_string(),
        format!("--python={}", python_executable.display()),
        format!("--target={}", warm.display()),
        "--require-hashes".to_string(),
        "--no-config".to_string(),
        format!("--default-index={index_url}"),
        format!("-r={}", resolved.display()),
    ];
    prime_checked(&install, &Run { cwd: stage, env: build_env, timeout: 600, boundary: Some(&boundary) }, package, target)
}

fn build_inner(
    package: &PackageVersion,
    target: &Target,
    source: &ReleaseFile,
    destination: &Path,
    stage: &Path,
    prebuild_cwd: &Path,
    opts: &BuildOptions<'_>,
) -> Result<PathBuf> {
    let ctx = |e: Error| e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
    let source_dir = stage.join("source");
    std::fs::create_dir(&source_dir).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
    let source_path = match download_artifact(source, &source_dir) {
        Ok(p) => p,
        Err(e) => return Err(ctx(e)),
    };
    build_source_file(package, target, &source_path, destination, stage, prebuild_cwd, opts)
}

fn build_source_file(
    package: &PackageVersion,
    target: &Target,
    source_path: &Path,
    destination: &Path,
    stage: &Path,
    prebuild_cwd: &Path,
    opts: &BuildOptions<'_>,
) -> Result<PathBuf> {
    let ctx = |e: Error| e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
    let suspicious = likely_binary(source_path, opts.likely_binary_ignore).map_err(ctx)?;
    let output = stage.join("wheels");
    std::fs::create_dir(&output).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
    let default_constraints;
    let constraints_path: &Path = match opts.build_constraints {
        Some(path) => path,
        None => {
            default_constraints = stage.join("build-constraints.txt");
            std::fs::write(&default_constraints, BUILD_CONSTRAINTS).map_err(|e| {
                ctx(Error::io(format!("could not stage the embedded build constraints at {}: {e}", default_constraints.display()), Stage::Build))
            })?;
            &default_constraints
        }
    };
    let build_requirements = sdist_build_requires(source_path).map_err(ctx)?;
    let build_temp = stage.join("tmp");
    std::fs::create_dir(&build_temp).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
    let base_env: FxHashMap<String, String> = match opts.env {
        Some(m) => m.clone(),
        None => std::env::vars().collect(),
    };
    let mut build_env = build_environment(&base_env, stage).map_err(|e| {
        ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build))
    })?;
    build_env.remove("MACOSX_DEPLOYMENT_TARGET");
    if target.platform.starts_with("macos-") {
        build_env.insert(
            "MACOSX_DEPLOYMENT_TARGET".to_string(),
            format!("{}.{}", MACOS_DEPLOYMENT_TARGET.0, MACOS_DEPLOYMENT_TARGET.1),
        );
    }
    build_env.insert("UV_CACHE_DIR".to_string(), stage.join("uv-cache").display().to_string());
    if !opts.custom_prebuild.is_empty() {
        let prefix = stage.join("prefix");
        std::fs::create_dir(&prefix).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
        for (command, _reason) in opts.custom_prebuild {
            let parts = shell_split(command);
            let args: Vec<String> = parts.into_iter().map(|a| a.replace("{prefix}", &prefix.display().to_string())).collect();
            if args.is_empty() || !command.contains("{prefix}") {
                return Err(ctx(Error::build("custom prebuild must use {prefix}", Stage::Build)));
            }
            run_checked(&args, &Run { cwd: stage, env: &build_env, timeout: 300, boundary: None }, package, target, Stage::Build)?;
        }
        let bin = prefix.join("bin").display().to_string();
        let old_path = build_env.get("PATH").cloned().unwrap_or_default();
        build_env.insert("PATH".to_string(), format!("{bin}{}", if old_path.is_empty() { String::new() } else { format!(":{}", old_path) }));
        let join = |a: &str, b: &str| if b.is_empty() { a.to_string() } else { format!("{a} {b}") };
        build_env.insert("CPPFLAGS".to_string(), join(&format!("-I{}", prefix.join("include").display()), build_env.get("CPPFLAGS").cloned().unwrap_or_default().as_str()));
        build_env.insert("LDFLAGS".to_string(), join(&format!("-L{}", prefix.join("lib").display()), build_env.get("LDFLAGS").cloned().unwrap_or_default().as_str()));
        build_env.insert("PKG_CONFIG_PATH".to_string(), {
            let old = build_env.get("PKG_CONFIG_PATH").cloned().unwrap_or_default();
            if old.is_empty() { prefix.join("lib/pkgconfig").display().to_string() } else { format!("{}:{old}", prefix.join("lib/pkgconfig").display()) }
        });
        build_env.insert("LD_LIBRARY_PATH".to_string(), {
            let old = build_env.get("LD_LIBRARY_PATH").cloned().unwrap_or_default();
            if old.is_empty() { prefix.join("lib").display().to_string() } else { format!("{}:{old}", prefix.join("lib").display()) }
        });
    }
    let build_path = build_env.get("PATH").cloned().unwrap_or_default();
    let python = interpreter(&crate::platforms::python_binary(), &build_path, Stage::Build).map_err(ctx)?;
    let mut readable = python.roots.clone();
    readable.extend(toolchain_read_roots(&base_env));
    readable.push(constraints_path.to_path_buf());
    prime_build_requirements(&build_requirements, constraints_path, &python.executable, &build_env, &readable, stage, opts.index_url, package, target)?;
    let boundary = Boundary { writable: vec![stage.to_path_buf()], readable, network: Network::Offline };
    let mut command: Vec<String> = vec![
        "uv".to_string(),
        "build".to_string(),
        "--no-config".to_string(),
        "--quiet".to_string(),
        "--wheel".to_string(),
        format!("--python={}", python.executable.display()),
        format!("--default-index={}", opts.index_url),
        "--index-strategy=first-index".to_string(),
        "--offline".to_string(),
        format!("--out-dir={}", output.display()),
    ];
    command.push("--build-constraints".to_string());
    command.push(constraints_path.display().to_string());
    if opts.require_build_hashes {
        command.push("--require-build-hashes".to_string());
        command.push("--preview-features".to_string());
        command.push("build-dependency-hashes".to_string());
    }
    command.push(source_path.display().to_string());
    run_checked(&command, &Run { cwd: stage, env: &build_env, timeout: 1800, boundary: Some(&boundary) }, package, target, Stage::Build)?;
    let mut wheels: Vec<PathBuf> = std::fs::read_dir(&output)
        .map_err(|e| ctx(Error::build(format!("could not build {package}: {e}"), Stage::Build)))?
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("whl"))
        .collect();
    wheels.sort();
    if wheels.len() != 1 {
        return Err(ctx(Error::build(format!("uv produced {} wheels for {package}", wheels.len()), Stage::Build)));
    }
    let mut wheel = wheels.pop().unwrap();
    validate_wheel_metadata_at(&wheel, package, target, opts.linux_repair_policy, false, Stage::Build).map_err(|r| {
        ctx(Error::new(r_category_kind(&r), r.detail.clone(), Stage::Build))
    })?;
    if let Some(reason) = suspicious.as_deref()
        && !produced_binary(&wheel).map_err(ctx)?
    {
        return Err(ctx(Error::build(
            format!("{package} likely contains compiled code but produced a pure wheel: {reason}"),
            Stage::Build,
        )));
    }
    let native = produced_binary(&wheel).map_err(ctx)?;
    if native {
        validate_wheel_architectures(&wheel, target).map_err(|r| {
            ctx(Error::new(r_category_kind(&r), r.detail.clone(), Stage::Build))
        })?;
        crate::platforms::verify_repair_tool(target, &build_env).map_err(ctx)?;
        let repair_output = stage.join("repaired");
        std::fs::create_dir(&repair_output).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Repair)))?;
        let repair = crate::platforms::repair_command(target, &wheel, &repair_output, opts.linux_repair_policy).map_err(ctx)?;
        run_checked(&repair, &Run { cwd: stage, env: &build_env, timeout: 600, boundary: None }, package, target, Stage::Repair)?;
        let mut repaired: Vec<PathBuf> = std::fs::read_dir(&repair_output)
            .map_err(|e| ctx(Error::build(format!("could not repair {package}: {e}"), Stage::Repair)))?
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("whl"))
            .collect();
        repaired.sort();
        if repaired.len() != 1 {
            return Err(ctx(Error::build(format!("repair produced {} wheels for {package}", repaired.len()), Stage::Repair)));
        }
        wheel = repaired.pop().unwrap();
    }
    let check_stage = if native { Stage::Repair } else { Stage::Build };
    validate_wheel_metadata_at(&wheel, package, target, opts.linux_repair_policy, true, check_stage).map_err(|r| {
        ctx(Error::new(r_category_kind(&r), r.detail.clone(), check_stage))
    })?;
    if native {
        validate_wheel_architectures(&wheel, target).map_err(|r| {
            ctx(Error::new(r_category_kind(&r), r.detail.clone(), Stage::Repair))
        })?;
        validate_repaired_policy(&wheel, package, target, opts.linux_repair_policy)?;
    }
    let final_path = destination.join(wheel.file_name().unwrap());
    match std::fs::hard_link(&wheel, &final_path) {
        Ok(()) => {}
        Err(_) => {
            let same = is_same_bytes(&final_path, &wheel);
            match same {
                Ok(true) => {}
                _ => {
                    return Err(ctx(Error::conflict(
                        format!("build output contains conflicting bytes: {}", final_path.display()),
                        Stage::Build,
                    )));
                }
            }
        }
    }
    if let Ok(dir) = std::fs::File::open(destination) {
        let _ = dir.sync_all();
    }
    let _ = std::fs::remove_file(&wheel);
    let _ = prebuild_cwd;
    Ok(final_path)
}

fn r_category_kind(r: &CompatibilityRejection) -> Kind {
    match r.category {
        CompatibilityCategory::PythonVersion | CompatibilityCategory::WheelTag | CompatibilityCategory::Architecture | CompatibilityCategory::RepairPolicy => Kind::Incompatible,
    }
}

fn is_same_bytes(a: &Path, b: &Path) -> Result<bool> {
    let ha = sha256_file(a)?;
    let hb = sha256_file(b)?;
    Ok(ha == hb)
}

struct Run<'a> {
    cwd: &'a Path,
    env: &'a FxHashMap<String, String>,
    timeout: u64,
    boundary: Option<&'a Boundary>,
}

fn run_checked(
    command: &[String],
    run: &Run<'_>,
    package: &PackageVersion,
    target: &Target,
    stage: Stage,
) -> Result<()> {
    let ctx = |e: Error| e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
    let confined;
    let argv = match run.boundary {
        Some(boundary) => {
            confined = confine(command, run.env, boundary, stage).map_err(ctx)?;
            &confined[..]
        }
        None => command,
    };
    match run_capture(argv, Some(run.cwd), run.env, None, run.timeout) {
        Ok(out) if out.success() => Ok(()),
        Ok(out) => {
            let mut detail = format!("could not {} {package}: {}", stage.as_str(), command.join(" "));
            let stderr = out.stderr.trim();
            if !stderr.is_empty() {
                let redacted = redacted_diagnostic(stderr, 2000);
                // decode bytes->replace already String
                detail = format!("{detail}: {redacted}");
            }
            Err(ctx(Error::build(detail, stage)))
        }
        Err(crate::process_policy::RunError::Timeout { stderr, .. }) => {
            let mut detail = format!("could not {} {package}: timed out", stage.as_str());
            if !stderr.trim().is_empty() {
                detail = format!("{detail}: {}", redacted_diagnostic(stderr.trim(), 2000));
            }
            Err(ctx(Error::build(detail, stage)))
        }
        Err(e) => Err(ctx(Error::build(format!("could not {} {package}: {e}", stage.as_str()), stage))),
    }
}

fn shell_split(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(cur.clone());
                    cur.clear();
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn validate_repaired_policy(wheel: &Path, package: &PackageVersion, target: &Target, linux_repair_policy: &str) -> Result<()> {
    let ctx = |e: Error| e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
    let filename = wheel.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let triples = crate::platforms::wheel_tag_triples(filename)
        .map_err(|_| ctx(Error::integrity(format!("repaired wheel has an invalid filename: {filename}"), Stage::Repair)))?;
    let _ = triples;
    let preference = crate::platforms::tag_preference(target, linux_repair_policy)
        .map_err(|e| ctx(Error::new(Kind::Incompatible, e.detail, Stage::Repair)))?;
    if crate::platforms::wheel_rank(filename, &preference).is_none() {
        return Err(ctx(Error::incompatible(
            format!(
                "repaired wheel tags do not satisfy {} for {}",
                crate::platforms::repair_policy(target, linux_repair_policy),
                target.platform
            ),
            Stage::Repair,
        )));
    }
    if target.platform.starts_with("macos-") {
        let (major, minor) = MACOS_DEPLOYMENT_TARGET;
        // check each plat tag matches macosx_X_Y with floor
        let stem = filename.strip_suffix(".whl").unwrap_or(filename);
        let parts: Vec<&str> = stem.split('-').collect();
        let plat_group = if parts.len() == 6 { parts[5] } else if parts.len() == 5 { parts[4] } else { "" };
        let mut any_macos = false;
        for tag in plat_group.split('.') {
            if let Some(rest) = tag.strip_prefix("macosx_") {
                any_macos = true;
                let nums: Vec<&str> = rest.split('_').collect();
                if nums.len() < 3 {
                    return Err(ctx(Error::incompatible(
                        format!("repaired macOS wheel does not retain the {major}.{minor} deployment floor"),
                        Stage::Repair,
                    )));
                }
                let x: u64 = nums[0].parse().unwrap_or(0);
                let y: u64 = nums[1].parse().unwrap_or(0);
                if (x, y) < (major, minor) {
                    return Err(ctx(Error::incompatible(
                        format!("repaired macOS wheel does not retain the {major}.{minor} deployment floor"),
                        Stage::Repair,
                    )));
                }
            }
        }
        if !any_macos {
            // non-macosx tags on macos target after repair: still require floor? delocate keeps tags; be lenient
        }
        let _ = any_macos;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn acquire_or_build(
    release: &Release,
    target: &Target,
    destination: &Path,
    opts: &BuildOptions<'_>,
    allow_yanked_reason: Option<&str>,
) -> Result<PathBuf> {
    let execution = target_python_version(target);
    let spec_ok = release.requires_python.trim().is_empty()
        || release.requires_python.parse::<pep440_rs::VersionSpecifiers>().is_ok_and(|s| s.contains(&execution));
    if !spec_ok {
        return Err(Error::incompatible(
            format!("release Requires-Python excludes CPython {execution}"),
            Stage::Acquisition,
        )
        .with_package(&release.package)
        .with_target(&target.python_version.to_string(), &target.platform));
    }
    let mut wheel_rejection: Option<CompatibilityRejection> = None;
    let mut wheel: Option<PathBuf> = None;
    match select_or_download(release, target, destination, opts.linux_repair_policy, allow_yanked_reason) {
        Ok(p) => wheel = p,
        Err(r) => {
            if r.detail.starts_with("ACQUISITION-ERROR: ") {
                let detail = r.detail["ACQUISITION-ERROR: ".len()..].to_string();
                // re-raise as generic build error preserving stage text
                let (stage_text, msg) = detail.split_once(": ").unwrap_or(("acquisition", detail.as_str()));
                let stage = match stage_text {
                    "build" => Stage::Build,
                    "repair" => Stage::Repair,
                    "validation" => Stage::Validation,
                    "publication" => Stage::Publication,
                    "planning" => Stage::Planning,
                    "configuration" => Stage::Configuration,
                    "command" => Stage::Command,
                    _ => Stage::Acquisition,
                };
                return Err(Error::new(Kind::Build, msg, stage)
                    .with_package(&release.package)
                    .with_target(&target.python_version.to_string(), &target.platform));
            }
            wheel_rejection = Some(r);
        }
    }
    if let Some(w) = wheel {
        return Ok(w);
    }
    let mut last_rejection: Option<CompatibilityRejection> = None;
    for source in &release.sdists {
        if let Some(rp) = &source.requires_python {
            let ok = rp.trim().is_empty() || rp.parse::<pep440_rs::VersionSpecifiers>().is_ok_and(|s| s.contains(&execution));
            if !ok {
                last_rejection = Some(CompatibilityRejection {
                    category: CompatibilityCategory::PythonVersion,
                    detail: format!("source archive Requires-Python excludes CPython {execution}"),
                    package: Some(release.package.clone()),
                    target: Some(target.clone()),
                });
                continue;
            }
        }
        match build_from_sdist(&release.package, target, source, destination, opts) {
            Ok(p) => return Ok(p),
            Err(e) if matches!(e.kind, Kind::Incompatible) => {
                last_rejection = Some(CompatibilityRejection {
                    category: CompatibilityCategory::WheelTag,
                    detail: e.detail.clone(),
                    package: Some(release.package.clone()),
                    target: Some(target.clone()),
                });
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(r) = last_rejection {
        return Err(Error::new(r_category_kind(&r), r.detail, Stage::Acquisition)
            .with_package(&release.package)
            .with_target(&target.python_version.to_string(), &target.platform));
    }
    if let Some(r) = wheel_rejection {
        return Err(Error::new(r_category_kind(&r), r.detail, Stage::Acquisition)
            .with_package(&release.package)
            .with_target(&target.python_version.to_string(), &target.platform));
    }
    Err(Error::missing_artifact(
        format!("no compatible wheel or source archive for {}", release.package),
        Stage::Acquisition,
    )
    .with_package(&release.package)
    .with_target(&target.python_version.to_string(), &target.platform))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_header_raw(name: &str, size: u64) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..262].copy_from_slice(b"ustar");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|&b| b as u32).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        header
    }

    fn raw_sdist(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, contents) in entries {
            out.extend_from_slice(&tar_header_raw(name, contents.len() as u64));
            out.extend_from_slice(contents);
            while !out.len().is_multiple_of(512) {
                out.push(0);
            }
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    fn write_gz(path: &Path, bytes: &[u8]) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap();
    }

    fn write_sdist(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    const PURE_BACKEND: &str = r#"
import base64, hashlib, zipfile
from pathlib import Path

def _row(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}"

def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    _probe()
    dist = "pure_probe-0.1.0.dist-info"
    files = {
        "pure_probe/__init__.py": b"VALUE = 1\n",
        f"{dist}/METADATA": b"Metadata-Version: 2.1\nName: pure-probe\nVersion: 0.1.0\nRequires-Python: >=3.9\n",
        f"{dist}/WHEEL": b"Wheel-Version: 1.0\nGenerator: probe\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    }
    rows = [_row(name, data) for name, data in sorted(files.items())]
    rows.append(f"{dist}/RECORD,,")
    files[f"{dist}/RECORD"] = ("\n".join(rows) + "\n").encode()
    name = "pure_probe-0.1.0-py3-none-any.whl"
    with zipfile.ZipFile(Path(wheel_directory) / name, "w") as wheel:
        for member, data in files.items():
            wheel.writestr(member, data)
    return name
"#;

    fn probe_function(action: &str, path: &Path) -> String {
        let attempt = match action {
            "read" => format!("open({:?}).read()", path.to_str().unwrap()),
            "write" => format!("open({:?}, 'w').write('x')", path.to_str().unwrap()),
            _ => return "def _probe():\n    pass\n".to_string(),
        };
        format!(
            "import os\n\n\ndef _evidence(text):\n    open(os.path.join(os.environ['TMPDIR'], 'evidence'), 'w').write(text)\n\n\ndef _probe():\n    try:\n        {attempt}\n    except OSError as error:\n        _evidence('boundary held: {action}: ' + type(error).__name__)\n        raise RuntimeError('boundary held') from None\n    _evidence('BREACH {action}')\n    raise RuntimeError('BREACH {action}')\n"
        )
    }

    fn pure_sdist(dir: &Path, action: &str, path: &Path) -> PathBuf {
        let backend = format!("{}\n{PURE_BACKEND}", probe_function(action, path));
        let sdist = dir.join("pure_probe-0.1.0.tar.gz");
        let pyproject: &[u8] = b"[build-system]\nrequires = []\nbuild-backend = \"backend\"\nbackend-path = [\".\"]\n\n[project]\nname = \"pure-probe\"\nversion = \"0.1.0\"\n";
        write_sdist(
            &sdist,
            &[
                ("pure_probe-0.1.0/pyproject.toml", pyproject),
                ("pure_probe-0.1.0/PKG-INFO", b"Metadata-Version: 2.1\nName: pure-probe\nVersion: 0.1.0\n"),
                ("pure_probe-0.1.0/backend.py", backend.as_bytes()),
            ],
        );
        sdist
    }

    fn host_target() -> Option<Target> {
        let output = std::process::Command::new(crate::platforms::python_binary())
            .args(["-c", "import sys; print(sys.implementation.name, sys.version_info.major, sys.version_info.minor)"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut parts = text.split_whitespace();
        let (implementation, major, minor) = (parts.next()?, parts.next()?, parts.next()?);
        if implementation != "cpython" {
            return None;
        }
        Target::new(&format!("{major}.{minor}"), &crate::platforms::current_platform()?).ok()
    }

    fn build_pure(dir: &Path, action: &str, path: &Path) -> Option<Result<PathBuf>> {
        let target = host_target()?;
        let uv_path = std::env::var("PATH").unwrap_or_default();
        crate::sandbox::resolve_program("uv", &uv_path)?;
        let sdist = pure_sdist(dir, action, path);
        let destination = tempfile::tempdir().unwrap();
        let wheels = destination.path().join("wheels");
        std::fs::create_dir(&wheels).unwrap();
        let package = PackageVersion::parse("pure-probe", "0.1.0").unwrap();
        let options = BuildOptions {
            index_url: "https://pypi.org/simple",
            likely_binary_ignore: &[],
            custom_prebuild: &[],
            linux_repair_policy: "manylinux_2_28",
            build_constraints: None,
            require_build_hashes: false,
            env: None,
            prebuild_cwd: Some(dir),
        };
        let built = build_source_file(&package, &target, &sdist, &wheels, dir, dir, &options);
        Some(built.map(|wheel| {
            let kept = dir.join(wheel.file_name().unwrap());
            std::fs::copy(&wheel, &kept).unwrap();
            kept
        }))
    }

    #[test]
    fn a_build_backend_cannot_read_publisher_files_or_write_outside_scratch() {
        if host_target().is_none() {
            eprintln!("skipping: needs a CPython interpreter on PATH");
            return;
        }
        let publisher = tempfile::tempdir().unwrap();
        let sentinel = publisher.path().join("sentinel");
        std::fs::write(&sentinel, "publisher secret").unwrap();
        let escape = publisher.path().join("escaped");
        for (action, probe) in [("read", &sentinel), ("write", &escape)] {
            let dir = tempfile::tempdir().unwrap();
            let control_dir = tempfile::tempdir().unwrap();
            std::fs::write(control_dir.path().join("backend.py"), format!("{}\n{PURE_BACKEND}", probe_function(action, probe))).unwrap();
            let control = std::process::Command::new(crate::platforms::python_binary())
                .env("TMPDIR", control_dir.path())
                .args(["-I", "-c", &format!("import sys; sys.path.insert(0, {:?}); import backend; backend.build_wheel({:?})", control_dir.path().to_str().unwrap(), control_dir.path().to_str().unwrap())])
                .output()
                .unwrap();
            assert!(!control.status.success(), "{control:?}");
            assert_eq!(std::fs::read_to_string(control_dir.path().join("evidence")).unwrap(), format!("BREACH {action}"));
            let _ = std::fs::remove_file(&escape);

            let Some(result) = build_pure(dir.path(), action, probe) else {
                eprintln!("skipping: needs uv on PATH");
                return;
            };

            assert!(result.is_err());
            let evidence = std::fs::read_to_string(dir.path().join("tmp/evidence")).unwrap();
            let denied = ["PermissionError", "FileNotFoundError"].iter().any(|kind| evidence == format!("boundary held: {action}: {kind}"));
            assert!(denied, "{evidence}");
        }
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "publisher secret");
        assert!(!escape.exists());
    }

    #[test]
    fn a_benign_sdist_builds_inside_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let Some(result) = build_pure(dir.path(), "none", dir.path()) else {
            eprintln!("skipping: needs uv and a CPython interpreter on PATH");
            return;
        };
        let wheel = result.unwrap();
        assert_eq!(wheel.file_name().and_then(|name| name.to_str()), Some("pure_probe-0.1.0-py3-none-any.whl"));
    }

    #[test]
    #[ignore = "requires uv, CPython, and network access to the build index; run by deploy/ecosystem-smoke.sh"]
    fn an_sdist_fetches_its_build_requirements_inside_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("fetched_probe-0.1.0.tar.gz");
        write_sdist(
            &sdist,
            &[
                ("fetched_probe-0.1.0/pyproject.toml", b"[build-system]\nrequires = [\"flit_core>=3.9,<4\"]\nbuild-backend = \"flit_core.buildapi\"\n\n[project]\nname = \"fetched-probe\"\nversion = \"0.1.0\"\ndescription = \"probe\"\n"),
                ("fetched_probe-0.1.0/PKG-INFO", b"Metadata-Version: 2.1\nName: fetched-probe\nVersion: 0.1.0\n"),
                ("fetched_probe-0.1.0/fetched_probe.py", b"VALUE = 1\n"),
            ],
        );
        let destination = tempfile::tempdir().unwrap();
        let target = host_target().expect("a CPython interpreter on PATH");
        let package = PackageVersion::parse("fetched-probe", "0.1.0").unwrap();
        let options = BuildOptions {
            index_url: "https://pypi.org/simple",
            likely_binary_ignore: &[],
            custom_prebuild: &[],
            linux_repair_policy: "manylinux_2_28",
            build_constraints: None,
            require_build_hashes: true,
            env: None,
            prebuild_cwd: Some(dir.path()),
        };

        let wheel = build_source_file(&package, &target, &sdist, destination.path(), dir.path(), dir.path(), &options).unwrap();

        assert_eq!(wheel.file_name().and_then(|name| name.to_str()), Some("fetched_probe-0.1.0-py2.py3-none-any.whl"));
    }

    #[test]
    fn build_requires_reads_the_sdist_build_system() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("probe-0.1.0.tar.gz");
        write_sdist(
            &sdist,
            &[
                ("probe-0.1.0/pyproject.toml", b"[build-system]\nrequires = [\"flit_core>=3.9,<4\", \"setuptools==80.0.0\"]\nbuild-backend = \"probe.backend\"\n"),
                ("probe-0.1.0/backend.py", b""),
            ],
        );
        assert_eq!(
            sdist_build_requires(&sdist).unwrap(),
            vec!["flit_core>=3.9,<4".to_string(), "setuptools==80.0.0".to_string()]
        );
    }

    #[test]
    fn build_requires_deduplicates_repeated_entries() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("probe-0.1.0.tar.gz");
        write_sdist(
            &sdist,
            &[("probe-0.1.0/pyproject.toml", b"[build-system]\nrequires = [\"flit_core>=3.9,<4\", \"flit_core>=3.9,<4\"]\n")],
        );
        assert_eq!(sdist_build_requires(&sdist).unwrap(), vec!["flit_core>=3.9,<4".to_string()]);
    }

    #[test]
    fn build_requires_falls_back_to_setuptools_without_a_build_system_table() {
        let dir = tempfile::tempdir().unwrap();
        let without_pyproject = dir.path().join("no-pyproject-0.1.0.tar.gz");
        write_sdist(&without_pyproject, &[("no_pyproject-0.1.0/setup.py", b"from setuptools import setup\nsetup()\n")]);
        let without_build_system = dir.path().join("no-build-system-0.1.0.tar.gz");
        write_sdist(
            &without_build_system,
            &[("no-build-system-0.1.0/pyproject.toml", b"[project]\nname = \"no-build-system\"\nversion = \"0.1.0\"\n")],
        );
        let legacy = LEGACY_BUILD_REQUIRES.iter().map(|value| value.to_string()).collect::<Vec<_>>();
        assert_eq!(sdist_build_requires(&without_pyproject).unwrap(), legacy);
        assert_eq!(sdist_build_requires(&without_build_system).unwrap(), legacy);
    }

    #[test]
    fn build_requires_honors_an_explicit_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("empty-requires-0.1.0.tar.gz");
        write_sdist(&sdist, &[("empty-requires-0.1.0/pyproject.toml", b"[build-system]\nrequires = []\nbuild-backend = \"backend\"\n")]);
        assert_eq!(sdist_build_requires(&sdist).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn build_requires_rejects_non_string_entries() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("non-string-0.1.0.tar.gz");
        write_sdist(&sdist, &[("non-string-0.1.0/pyproject.toml", b"[build-system]\nrequires = [1]\n")]);
        let error = sdist_build_requires(&sdist).unwrap_err();
        assert!(format!("{error}").contains("array of strings"), "{error}");
    }

    #[test]
    fn build_requires_rejects_a_multiline_requirement() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("injected-0.1.0.tar.gz");
        write_sdist(&sdist, &[("injected-0.1.0/pyproject.toml", b"[build-system]\nrequires = [\"\"\"flit_core\nsetuptools\"\"\"]\n")]);
        let error = sdist_build_requires(&sdist).unwrap_err();
        assert!(format!("{error}").contains("multiple lines"), "{error}");
    }

    #[test]
    fn build_requires_rejects_malformed_toml() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("malformed-0.1.0.tar.gz");
        write_sdist(&sdist, &[("malformed-0.1.0/pyproject.toml", b"[build-system\nrequires = [1]\n")]);
        let error = sdist_build_requires(&sdist).unwrap_err();
        assert!(format!("{error}").contains("not valid TOML"), "{error}");
    }

    #[test]
    fn embedded_build_constraints_pin_hashes_for_every_requirement() {
        let mut pins = 0usize;
        let mut current_hashes = 0usize;
        let mut current_pin = String::new();
        let mut unhashed = Vec::new();
        for line in BUILD_CONSTRAINTS.lines() {
            let token = line.split_whitespace().next().unwrap_or("");
            if token.contains("==") {
                if pins > 0 && current_hashes == 0 {
                    unhashed.push(std::mem::take(&mut current_pin));
                }
                current_pin = token.to_string();
                current_hashes = 0;
                pins += 1;
                continue;
            }
            if line.contains("--hash=sha256:") {
                current_hashes += 1;
            }
        }
        if pins > 0 && current_hashes == 0 {
            unhashed.push(current_pin);
        }
        assert!(pins > 0, "deploy/build-constraints.txt contains no pins");
        assert!(unhashed.is_empty(), "pins without a sha256 hash: {unhashed:?}");
    }

    #[test]
    fn validates_build_index_forms() {
        assert!(validate_build_index("https://pypi.org/simple", false).is_ok());
        assert!(validate_build_index("http://127.0.0.1:8000/simple", false).is_ok());
        assert!(validate_build_index("http://localhost:8000/simple", false).is_ok());
        assert!(validate_build_index("http://packages.example/simple", false).is_err());
        assert!(validate_build_index("https://user@pypi.org/simple", false).is_err());
        assert!(validate_build_index("https://pypi.org/simple", true).is_err());
        assert!(validate_build_index("https://index.example.test/simple", true).is_ok());
    }

    #[test]
    fn requires_native_runner() {
        let current = crate::platforms::current_platform().unwrap();
        let ok = Target::new("3.15", &current).unwrap();
        assert!(require_native_target(&ok).is_ok());
        let other = if current.starts_with("linux") { "macos-arm64" } else { "linux-musl-x86_64" };
        let bad = Target::new("3.15", other).unwrap();
        assert!(require_native_target(&bad).is_err());
    }

    #[test]
    fn build_environment_seeds_the_installed_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(origin.join(".cargo/bin")).unwrap();
        std::fs::write(origin.join(".cargo/bin/cargo"), b"cargo").unwrap();
        std::fs::write(origin.join(".cargo/credentials.toml"), b"secret").unwrap();
        std::fs::create_dir_all(origin.join(".rustup/toolchains/nightly/bin")).unwrap();
        std::fs::write(origin.join(".rustup/toolchains/nightly/bin/rustc"), b"rustc").unwrap();
        std::fs::write(
            origin.join(".rustup/settings.toml"),
            b"default_toolchain = \"nightly\"\n",
        )
        .unwrap();
        std::fs::create_dir(&stage).unwrap();
        let base: FxHashMap<String, String> = [
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), origin.display().to_string()),
        ]
        .into_iter()
        .collect();

        let env = build_environment(&base, &stage).unwrap();
        let home = stage.join("home");
        let cargo_home = home.join(".cargo");
        let rustup_home = home.join(".rustup");
        assert_eq!(env.get("HOME").map(String::as_str), Some(home.to_str().unwrap()));
        assert_eq!(
            env.get("CARGO_HOME").map(String::as_str),
            Some(cargo_home.to_str().unwrap())
        );
        assert_eq!(
            env.get("RUSTUP_HOME").map(String::as_str),
            Some(rustup_home.to_str().unwrap())
        );
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some(format!("{}:/usr/bin", cargo_home.join("bin").display()).as_str())
        );
        assert_eq!(
            std::fs::read_link(cargo_home.join("bin/cargo")).unwrap(),
            origin.join(".cargo/bin/cargo")
        );
        assert_eq!(
            std::fs::read_link(rustup_home.join("toolchains/nightly")).unwrap(),
            origin.join(".rustup/toolchains/nightly")
        );
        assert_eq!(
            std::fs::read_to_string(rustup_home.join("settings.toml")).unwrap(),
            "default_toolchain = \"nightly\"\n"
        );
        assert!(!cargo_home.join("credentials.toml").exists());
        assert!(!env.values().any(|value| value.contains(&origin.display().to_string())));
    }

    #[test]
    fn build_environment_honors_explicit_toolchain_homes() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin");
        let cargo_home = origin.join("custom-cargo");
        let rustup_home = origin.join("custom-rustup");
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(cargo_home.join("bin")).unwrap();
        std::fs::write(cargo_home.join("bin/cargo"), b"cargo").unwrap();
        std::fs::create_dir_all(rustup_home.join("toolchains/stable/bin")).unwrap();
        std::fs::write(rustup_home.join("toolchains/stable/bin/rustc"), b"rustc").unwrap();
        std::fs::create_dir(&stage).unwrap();
        let base: FxHashMap<String, String> = [
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), origin.display().to_string()),
            ("CARGO_HOME".to_string(), cargo_home.display().to_string()),
            ("RUSTUP_HOME".to_string(), rustup_home.display().to_string()),
        ]
        .into_iter()
        .collect();

        let env = build_environment(&base, &stage).unwrap();
        let home = stage.join("home");
        assert_eq!(
            std::fs::read_link(home.join(".cargo/bin/cargo")).unwrap(),
            cargo_home.join("bin/cargo")
        );
        assert_eq!(
            std::fs::read_link(home.join(".rustup/toolchains/stable")).unwrap(),
            rustup_home.join("toolchains/stable")
        );
        assert_eq!(
            env.get("CARGO_HOME").map(String::as_str),
            Some(home.join(".cargo").to_str().unwrap())
        );
        assert_eq!(
            env.get("RUSTUP_HOME").map(String::as_str),
            Some(home.join(".rustup").to_str().unwrap())
        );
    }

    #[test]
    #[ignore = "requires uv, a native compiler, and repair tooling; run by deploy/test-mypypi-native.sh"]
    fn native_sdist_builds_a_repaired_extension_wheel() {
        let state = PathBuf::from(std::env::var_os("MYPYPI_NATIVE_STATE").expect(
            "MYPYPI_NATIVE_STATE must name the state directory that should receive the built wheel",
        ));
        let wheels = state.join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("native_probe-0.1.0.tar.gz");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/mypypi/native-sdist");
        let names = ["pyproject.toml", "PKG-INFO", "backend.py", "src/native_probe.c", "src/native_probe_cpp.cpp"];
        let contents: Vec<Vec<u8>> = names
            .iter()
            .map(|name| std::fs::read(fixture.join(name)).unwrap())
            .collect();
        let prefixed: Vec<String> = names
            .iter()
            .map(|name| format!("native_probe-0.1.0/{name}"))
            .collect();
        let entries: Vec<(&str, &[u8])> = prefixed
            .iter()
            .map(String::as_str)
            .zip(contents.iter().map(Vec::as_slice))
            .collect();
        write_sdist(&sdist, &entries);

        let platform = crate::platforms::current_platform().unwrap();
        let target = Target::new("3.15", &platform).unwrap();
        let package = PackageVersion::parse("native-probe", "0.1.0").unwrap();
        let options = BuildOptions {
            index_url: "https://pypi.org/simple",
            likely_binary_ignore: &[],
            custom_prebuild: &[],
            linux_repair_policy: "manylinux_2_28",
            build_constraints: None,
            require_build_hashes: false,
            env: None,
            prebuild_cwd: Some(dir.path()),
        };
        let wheel = build_source_file(
            &package,
            &target,
            &sdist,
            &wheels,
            dir.path(),
            dir.path(),
            &options,
        )
        .unwrap();
        let name = wheel.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("native_probe-0.1.0-cp315-cp315-"), "{name}");
        if platform.starts_with("linux-musl-") {
            assert!(name.contains("-musllinux_1_2_"), "{name}");
        } else if platform.starts_with("linux") {
            assert!(name.contains("-manylinux_2_28_"), "{name}");
        } else {
            assert!(name.contains("-macosx_15_0_"), "{name}");
        }
        let archive = crate::wheel_zip::Archive::open(&wheel).unwrap();
        let members: Vec<String> = archive.names().map(str::to_string).collect();
        assert!(members.iter().any(|member| member.starts_with("native_probe.cpython-315")), "{members:?}");
        assert!(members.iter().any(|member| member.starts_with("native_probe_cpp.cpython-315")), "{members:?}");
        let validated = crate::validate::validate_wheel(
            &wheel,
            &target,
            &wheels,
            &crate::validate::ValidationPolicy::default(),
            "manylinux_2_28",
        )
        .unwrap();
        assert_eq!(validated.imports, ["native_probe", "native_probe_cpp"]);
    }

    #[test]
    #[ignore = "requires uv and the host Rust toolchain; run by deploy/ecosystem-smoke.sh"]
    fn rust_sdist_build_finds_the_host_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let sdist = dir.path().join("rust-toolchain-smoke-0.1.0.tar.gz");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/mypypi/rust-toolchain-sdist");
        let pyproject = std::fs::read(fixture.join("pyproject.toml")).unwrap();
        let backend = std::fs::read(fixture.join("backend.py")).unwrap();
        let pkg_info: &[u8] = b"Metadata-Version: 2.1\nName: rust-toolchain-smoke\nVersion: 0.1.0\n";
        write_sdist(
            &sdist,
            &[
                ("rust-toolchain-smoke-0.1.0/pyproject.toml", pyproject.as_slice()),
                ("rust-toolchain-smoke-0.1.0/PKG-INFO", pkg_info),
                ("rust-toolchain-smoke-0.1.0/backend.py", backend.as_slice()),
            ],
        );
        let stage = dir.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        std::fs::create_dir(stage.join("tmp")).unwrap();
        let base: FxHashMap<String, String> = std::env::vars().collect();
        let mut env = build_environment(&base, &stage).unwrap();
        env.insert(
            "RUSTUP_DIST_SERVER".to_string(),
            "http://127.0.0.1:1".to_string(),
        );
        let out_dir = stage.join("wheels");
        std::fs::create_dir(&out_dir).unwrap();
        let path = env.get("PATH").cloned().unwrap_or_default();
        let python = interpreter(&crate::platforms::python_binary(), &path, Stage::Build).unwrap();
        let mut readable = python.roots.clone();
        readable.extend(toolchain_read_roots(&base));
        let boundary = Boundary { writable: vec![dir.path().to_path_buf()], readable, network: Network::Online };
        let command = vec![
            "uv".to_string(),
            "build".to_string(),
            "--no-config".to_string(),
            "--wheel".to_string(),
            "--quiet".to_string(),
            format!("--python={}", python.executable.display()),
            format!("--out-dir={}", out_dir.display()),
            sdist.display().to_string(),
        ];
        let confined = confine(&command, &env, &boundary, Stage::Build).unwrap();
        let out = run_capture(&confined, Some(&stage), &env, None, 300).unwrap();
        assert!(out.success(), "uv build failed: {}", out.stderr);
        let wheels: Vec<_> = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("whl"))
            .collect();
        assert_eq!(wheels.len(), 1);
    }

    #[test]
    fn rejects_sdist_member_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0.tar.gz");
        write_gz(&path, &raw_sdist(&[("../escape.txt", b"owned")]));
        let err = likely_binary(&path, &[]).unwrap_err();
        assert!(format!("{err}").contains("unsafe sdist member"), "{err}");
    }

    #[test]
    fn rejects_sdist_expanded_size_over_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0.tar.gz");
        let mut raw = tar_header_raw("pkg-1.0/big", SDIST_EXPANDED_LIMIT + 1);
        raw.extend_from_slice(&[0u8; 1024]);
        write_gz(&path, &raw);
        let err = likely_binary(&path, &[]).unwrap_err();
        assert!(format!("{err}").contains("expanded size exceeds"), "{err}");
    }

    #[test]
    fn rejects_duplicate_sdist_members() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0.tar.gz");
        write_sdist(
            &path,
            &[("pkg-1.0/module.py", b"x"), ("pkg-1.0/module.py", b"y")],
        );
        let err = likely_binary(&path, &[]).unwrap_err();
        assert!(format!("{err}").contains("duplicate paths"), "{err}");
    }

    #[test]
    fn inspects_valid_tar_sdist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0.tar.gz");
        write_sdist(
            &path,
            &[("pkg-1.0/setup.py", b"from cffi import FFI\ncffi_modules=['build.py:ffi']\n")],
        );
        assert_eq!(
            likely_binary(&path, &[]).unwrap().as_deref(),
            Some("source archive setup.py declares cffi_modules")
        );
    }
}
