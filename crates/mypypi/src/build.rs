#![forbid(unsafe_code)]

use crate::acquire::{
    CompatibilityCategory, CompatibilityRejection, Release, ReleaseFile, download_artifact,
    select_or_download, validate_wheel_architectures, validate_wheel_metadata_at, sha256_file,
};
use crate::error::{Error, Kind, Result, Stage};
use crate::models::{PackageVersion, Target};
use crate::platforms::{MACOS_DEPLOYMENT_TARGET, native_architectures_from_bytes, target_python_version};
use crate::process_policy::{redacted_diagnostic, run_capture, uv_environment};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

const BINARY_EXTENSIONS: &[&str] = &[".c", ".cc", ".cpp", ".cxx", ".pxd", ".pxi", ".pyx", ".go", ".rs"];
const BINARY_SUFFIXES: &[&str] = &[".so", ".dylib", ".pyd", ".dll", ".exe"];

pub fn validate_build_index(url: &str, strict: bool) -> Result<String> {
    let parsed: url::Url = url.parse().map_err(|_| {
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
    let remote_https = parsed.scheme() == "https" && parsed.has_host();
    let local_http = parsed.scheme() == "http" && is_loopback_host && parsed.has_host();
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
    let host_family = std::env::consts::OS;
    let mut host_arch = std::env::consts::ARCH.to_string();
    if host_family == "macos" && host_arch == "aarch64" {
        host_arch = "arm64".to_string();
    } else if host_family == "linux" && host_arch == "amd64" {
        host_arch = "x86_64".to_string();
    } else if host_family == "linux" && host_arch == "arm64" {
        host_arch = "aarch64".to_string();
    }
    let expected = format!("{host_family}-{host_arch}");
    if target.platform != expected {
        return Err(Error::incompatible(
            format!("native builds require a matching host; host is {expected}, target is {}", target.platform),
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
    let ignored: std::collections::BTreeSet<&str> = ignored_paths.iter().map(|s| s.as_str()).collect();
    // zip sdist?
    let is_zip = zip::ZipArchive::new(std::fs::File::open(sdist).map_err(|e| {
        Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
    })?)
    .is_ok();
    void_is_zip_check(sdist, is_zip)?;
    let mut extensions: BTreeMap<String, bool> = BTreeMap::new();
    let mut native_payloads: BTreeMap<String, bool> = BTreeMap::new();
    let mut setup_py: Vec<u8> = Vec::new();
    if is_zip {
        let file = std::fs::File::open(sdist).map_err(|e| {
            Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
        })?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| {
            Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
        })?;
        let mut names = Vec::new();
        for i in 0..archive.len() {
            let f = archive.by_index(i).map_err(|e| {
                Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build)
            })?;
            names.push(f.name().to_string());
        }
        let res = {
            let mut seen = std::collections::BTreeSet::new();
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
                let mut buf = Vec::new();
                archive.by_name(name).map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?.read_to_end(&mut buf).map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?;
                setup_py.extend_from_slice(&buf);
            }
            if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str()) {
                let dot = format!(".{}", ext.to_lowercase());
                if BINARY_EXTENSIONS.contains(&dot.as_str()) {
                    extensions.insert(dot, true);
                }
            }
            let mut content = Vec::new();
            archive.by_name(name).map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?.take(1024 * 1024).read_to_end(&mut content).map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?;
            if !native_architectures_from_bytes(&content).map_err(|e| {
                Error::integrity(format!("malformed native header in {name}: {e}"), Stage::Build)
            })?.is_empty()
            {
                native_payloads.insert(name.clone(), true);
            }
        }
    } else {
        // tarball: extract to temp dir with `tar` and walk
        let dir_path = crate::process_policy::make_temp_dir(".mypypi-sdist-").map_err(|e| Error::build(format!("cannot inspect source archive: {e}"), Stage::Build))?;
        let _dir_guard = TempDirGuard { path: dir_path.clone() };
        let dir: std::path::PathBuf = dir_path;
        let status = std::process::Command::new("tar")
            .args(["-xzf"])
            .arg(sdist)
            .arg("-C")
            .arg(&dir)
            .status()
            .map_err(|e| Error::build(format!("cannot inspect source archive {sdist}: {e}", sdist = sdist.display()), Stage::Build))?;
        if !status.success() {
            return Err(Error::build(
                format!("cannot inspect source archive {sdist}: tar extraction failed", sdist = sdist.display()),
                Stage::Build,
            ));
        }
        let mut files = Vec::new();
        collect_files(&dir, &dir, &mut files).map_err(|e| {
            Error::build(format!("cannot inspect source archive: {e}"), Stage::Build)
        })?;
        files.sort();
        let res = {
            let mut seen = std::collections::BTreeSet::new();
            files.iter().any(|n| !seen.insert(n.clone()))
        }; if res {
            return Err(Error::build("cannot inspect source archive: source archive contains duplicate paths", Stage::Build));
        }
        for rel in &files {
            if !safe_member(rel) {
                return Err(Error::build(format!("cannot inspect source archive: unsafe sdist member: {rel}"), Stage::Build));
            }
        }
        for rel in &files {
            if rel.contains("/test/") || rel.contains("/tests/") || ignored.contains(rel.as_str()) {
                continue;
            }
            let full = dir.join(rel);
            if full.is_dir() {
                continue;
            }
            if rel.ends_with("/setup.py")
                && let Ok(bytes) = std::fs::read(&full) {
                    setup_py.extend_from_slice(&bytes);
                }
            if let Some(ext) = Path::new(rel).extension().and_then(|e| e.to_str()) {
                let dot = format!(".{}", ext.to_lowercase());
                if BINARY_EXTENSIONS.contains(&dot.as_str()) {
                    extensions.insert(dot, true);
                }
            }
            let content = read_head(&full, 1024 * 1024);
            if !native_architectures_from_bytes(&content).map_err(|e| {
                Error::integrity(format!("malformed native header in {rel}: {e}"), Stage::Build)
            })?.is_empty()
            {
                native_payloads.insert(rel.clone(), true);
            }
        }
    }
    if !extensions.is_empty() {
        let list: Vec<&String> = extensions.keys().collect();
        let joined = list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        return Ok(Some(format!("source archive contains compiled-source files: {joined}")));
    }
    if !native_payloads.is_empty() {
        let list: Vec<&String> = native_payloads.keys().collect();
        let joined = list.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        return Ok(Some(format!("source archive contains native payloads: {joined}")));
    }
    if contains_bytes(&setup_py, b"cffi_modules") {
        return Ok(Some("source archive setup.py declares cffi_modules".to_string()));
    }
    Ok(None)
}

struct TempDirGuard {
    path: std::path::PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn void_is_zip_check(_sdist: &Path, _is_zip: bool) -> Result<()> {
    Ok(())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let rel = entry.path().strip_prefix(root).unwrap_or(&entry.path()).to_string_lossy().replace('\\', "/");
        // guard: extracted paths must stay inside root
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let canonical = entry.path().canonicalize().unwrap_or_else(|_| entry.path());
        if !canonical.starts_with(&canonical_root) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "tar escaped root"));
        }
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsupported source archive member: {rel}")));
        }
        if ft.is_dir() {
            collect_files(root, &entry.path(), out)?;
        } else if ft.is_file() {
            out.push(rel);
        } else {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsupported source archive member: {rel}")));
        }
    }
    Ok(())
}

fn read_head(path: &Path, limit: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(limit as u64).read_to_end(&mut buf);
    }
    buf
}

pub fn produced_binary(wheel: &Path) -> Result<bool> {
    let file = std::fs::File::open(wheel).map_err(|e| {
        Error::build(format!("cannot inspect built wheel {wheel}: {e}", wheel = wheel.display()), Stage::Build)
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        Error::build(format!("cannot inspect built wheel {wheel}: {e}", wheel = wheel.display()), Stage::Build)
    })?;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| {
            Error::build(format!("cannot inspect built wheel: {e}"), Stage::Build)
        })?;
        names.push(f.name().to_string());
    }
    let res = {
        let mut seen = std::collections::BTreeSet::new();
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
        let mut content = Vec::new();
        archive.by_name(name).map_err(|e| Error::build(format!("cannot inspect built wheel: {e}"), Stage::Build))?.take(1024 * 1024).read_to_end(&mut content).map_err(|e| Error::build(format!("cannot inspect built wheel: {e}"), Stage::Build))?;
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
    pub env: Option<&'a BTreeMap<String, String>>,
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
    let suspicious = likely_binary(&source_path, opts.likely_binary_ignore).map_err(ctx)?;
    let output = stage.join("wheels");
    std::fs::create_dir(&output).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
    let build_temp = stage.join("tmp");
    std::fs::create_dir(&build_temp).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
    let base_env: BTreeMap<String, String> = match opts.env {
        Some(m) => m.clone(),
        None => std::env::vars().collect(),
    };
    let mut build_env = uv_environment(Some(&base_env));
    build_env.insert("ARCHFLAGS".to_string(), String::new());
    build_env.insert("TMPDIR".to_string(), build_temp.display().to_string());
    build_env.insert("TEMP".to_string(), build_temp.display().to_string());
    build_env.insert("TMP".to_string(), build_temp.display().to_string());
    build_env.remove("MACOSX_DEPLOYMENT_TARGET");
    if target.platform.starts_with("macos-") {
        build_env.insert(
            "MACOSX_DEPLOYMENT_TARGET".to_string(),
            format!("{}.{}", MACOS_DEPLOYMENT_TARGET.0, MACOS_DEPLOYMENT_TARGET.1),
        );
    }
    if !opts.custom_prebuild.is_empty() {
        let prefix = stage.join("prefix");
        std::fs::create_dir(&prefix).map_err(|e| ctx(Error::io(format!("could not create build staging directory: {e}"), Stage::Build)))?;
        for (command, _reason) in opts.custom_prebuild {
            let parts = shell_split(command);
            let args: Vec<String> = parts.into_iter().map(|a| a.replace("{prefix}", &prefix.display().to_string())).collect();
            if args.is_empty() || !command.contains("{prefix}") {
                return Err(ctx(Error::build("custom prebuild must use {prefix}", Stage::Build)));
            }
            run_checked(&args, stage, &build_env, 300, package, target, Stage::Build)?;
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
    let python = crate::platforms::python_binary();
    let mut command: Vec<String> = vec![
        "uv".to_string(),
        "build".to_string(),
        "--no-config".to_string(),
        "--quiet".to_string(),
        "--wheel".to_string(),
        format!("--python={python}"),
        format!("--default-index={}", opts.index_url),
        "--index-strategy=first-index".to_string(),
        format!("--out-dir={}", output.display()),
    ];
    if let Some(constraints) = opts.build_constraints {
        command.push("--build-constraints".to_string());
        command.push(constraints.display().to_string());
    }
    if opts.require_build_hashes {
        command.push("--require-build-hashes".to_string());
    }
    command.push(source_path.display().to_string());
    run_checked(&command, stage, &build_env, 1800, package, target, Stage::Build)?;
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
        run_checked(&repair, stage, &build_env, 600, package, target, Stage::Repair)?;
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

fn run_checked(
    command: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout: u64,
    package: &PackageVersion,
    target: &Target,
    stage: Stage,
) -> Result<()> {
    let ctx = |e: Error| e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
    match run_capture(command, Some(cwd), env, None, timeout) {
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
            format!("repaired wheel tags do not satisfy {linux_repair_policy} for {}", target.platform),
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
        let other = if current.starts_with("linux") { "macos-arm64" } else { "linux-x86_64" };
        let bad = Target::new("3.15", other).unwrap();
        assert!(require_native_target(&bad).is_err());
    }
}
