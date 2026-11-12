#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};
use crate::models::Target;
use rustc_hash::{FxHashMap, FxHashSet};
use std::str::FromStr;
use std::sync::OnceLock;

pub const MACOS_DEPLOYMENT_TARGET: (u64, u64) = (15, 0);
pub const MUSLLINUX_POLICY: &str = "musllinux_1_2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Linux,
    Musllinux,
    Macos,
}

fn family_of(platform: &str) -> Result<Family> {
    match platform {
        "linux-x86_64" | "linux-aarch64" => Ok(Family::Linux),
        "linux-musl-x86_64" | "linux-musl-aarch64" => Ok(Family::Musllinux),
        "macos-x86_64" | "macos-arm64" => Ok(Family::Macos),
        _ => Err(Error::new(
            Kind::Incompatible,
            format!("unsupported build target {platform}"),
            Stage::Build,
        )),
    }
}

fn arch_of(platform: &str) -> &str {
    match platform {
        "linux-x86_64" | "linux-musl-x86_64" | "macos-x86_64" => "x86_64",
        "linux-aarch64" | "linux-musl-aarch64" => "aarch64",
        "macos-arm64" => "arm64",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxLibc {
    Glibc,
    Musl,
}

fn platform_name(os: &str, arch: &str, libc: LinuxLibc) -> Option<String> {
    match (os, arch, libc) {
        ("macos", "aarch64", _) => Some("macos-arm64".to_string()),
        ("macos", "x86_64", _) => Some("macos-x86_64".to_string()),
        ("linux", "x86_64", LinuxLibc::Glibc) => Some("linux-x86_64".to_string()),
        ("linux", "aarch64", LinuxLibc::Glibc) => Some("linux-aarch64".to_string()),
        ("linux", "x86_64", LinuxLibc::Musl) => Some("linux-musl-x86_64".to_string()),
        ("linux", "aarch64", LinuxLibc::Musl) => Some("linux-musl-aarch64".to_string()),
        _ => None,
    }
}

fn elf_interpreter(content: &[u8]) -> Option<&[u8]> {
    if content.len() < 64 || !content.starts_with(b"\x7fELF") || content[4] != 2 {
        return None;
    }
    let little = match content[5] {
        1 => true,
        2 => false,
        _ => return None,
    };
    let rd16 = |at: usize| -> Option<usize> {
        let b: [u8; 2] = content.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(usize::from(if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }))
    };
    let rd32 = |at: usize| -> Option<usize> {
        let b: [u8; 4] = content.get(at..at.checked_add(4)?)?.try_into().ok()?;
        usize::try_from(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }).ok()
    };
    let rd64 = |at: usize| -> Option<usize> {
        let b: [u8; 8] = content.get(at..at.checked_add(8)?)?.try_into().ok()?;
        usize::try_from(if little { u64::from_le_bytes(b) } else { u64::from_be_bytes(b) }).ok()
    };
    let table = rd64(32)?;
    let entry_size = rd16(54)?;
    let count = rd16(56)?;
    if entry_size < 56 {
        return None;
    }
    for index in 0..count {
        let entry = table.checked_add(index.checked_mul(entry_size)?)?;
        if rd32(entry)? == 3 {
            let offset = rd64(entry.checked_add(8)?)?;
            let size = rd64(entry.checked_add(32)?)?;
            let bytes = content.get(offset..offset.checked_add(size)?)?;
            return Some(bytes.split(|b| *b == 0).next().unwrap_or(bytes));
        }
    }
    None
}

fn libc_of_executable(content: &[u8]) -> Option<LinuxLibc> {
    let interpreter = elf_interpreter(content)?;
    let name = interpreter.rsplit(|b| *b == b'/').next().unwrap_or(interpreter);
    Some(if name.starts_with(b"ld-musl-") { LinuxLibc::Musl } else { LinuxLibc::Glibc })
}

fn host_linux_libc() -> LinuxLibc {
    use std::io::Read as _;
    let Some(python) = path_lookup(&python_binary()) else {
        return LinuxLibc::Glibc;
    };
    let mut head = Vec::new();
    let read = std::fs::File::open(python).and_then(|file| file.take(64 * 1024).read_to_end(&mut head));
    match read {
        Ok(_) => libc_of_executable(&head).unwrap_or(LinuxLibc::Glibc),
        Err(_) => LinuxLibc::Glibc,
    }
}

pub fn current_platform() -> Option<String> {
    let libc = if std::env::consts::OS == "linux" { host_linux_libc() } else { LinuxLibc::Glibc };
    platform_name(std::env::consts::OS, std::env::consts::ARCH, libc)
}

fn probed_micro(major: u64, minor: u64) -> Option<u64> {
    static CACHE: OnceLock<Option<(u64, u64, u64)>> = OnceLock::new();
    let cached = CACHE.get_or_init(|| {
        let out = std::process::Command::new(python_binary())
            .args(["-c", "import sys; print(sys.version_info.major, sys.version_info.minor, sys.version_info.micro)"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let mut parts = text.split_whitespace().filter_map(|p| p.parse::<u64>().ok());
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(a), Some(b), Some(c)) => Some((a, b, c)),
                    _ => None,
                }
            }
            _ => None,
        }
    });
    match *cached {
        Some((a, b, c)) if a == major && b == minor => Some(c),
        _ => None,
    }
}

pub fn target_python_version(target: &Target) -> pep440_rs::Version {
    let release = target.python_version.release();
    if release.len() < 2 {
        return target.python_version.clone();
    }
    let (major, minor) = (release[0], release[1]);
    if current_platform().as_deref() == Some(target.platform.as_str())
        && let Some(micro) = probed_micro(major, minor)
            && let Ok(v) = pep440_rs::Version::from_str(&format!("{major}.{minor}.{micro}")) {
                return v;
            }
    pep440_rs::Version::new([major, minor, 0])
}

fn mac_binary_formats(version: (u64, u64), arch: &str) -> Vec<String> {
    let mut formats = vec![arch.to_string()];
    if arch == "x86_64" {
        if version < (10, 4) {
            return Vec::new();
        }
        formats.extend(["intel".to_string(), "fat64".to_string(), "fat32".to_string()]);
    }
    if arch == "arm64" || arch == "x86_64" {
        formats.push("universal2".to_string());
    }
    if arch == "x86_64" {
        formats.push("universal".to_string());
    }
    formats
}

pub fn repair_policy<'a>(target: &Target, linux_repair_policy: &'a str) -> &'a str {
    match family_of(target.platform.as_str()) {
        Ok(Family::Musllinux) => MUSLLINUX_POLICY,
        _ => linux_repair_policy,
    }
}

pub fn target_platform_tags(target: &Target, linux_repair_policy: &str) -> Result<Vec<String>> {
    match family_of(target.platform.as_str())? {
        Family::Musllinux => {
            let arch = arch_of(target.platform.as_str());
            Ok((0..=2).rev().map(|minor| format!("musllinux_1_{minor}_{arch}")).collect())
        }
        Family::Linux => {
            let rest = linux_repair_policy
                .strip_prefix("manylinux_2_")
                .ok_or_else(|| Error::validation("Linux repair policy must be a manylinux_2_<minor> platform tag", Stage::Build))?;
            let floor: u64 = rest.parse().map_err(|_| {
                Error::validation("Linux repair policy must be a manylinux_2_<minor> platform tag", Stage::Build)
            })?;
            if floor < 5 {
                return Err(Error::validation(
                    "Linux repair policy must be at least manylinux_2_5",
                    Stage::Build,
                ));
            }
            let arch = arch_of(target.platform.as_str());
            let mut out = Vec::new();
            for minor in (5..=floor).rev() {
                out.push(format!("manylinux_2_{minor}_{arch}"));
            }
            for (minor, name) in [(17, "manylinux2014"), (12, "manylinux2010"), (5, "manylinux1")] {
                if floor >= minor {
                    out.push(format!("{name}_{arch}"));
                }
            }
            Ok(out)
        }
        Family::Macos => {
            let arch = arch_of(target.platform.as_str());
            let (floor_major, _) = MACOS_DEPLOYMENT_TARGET;
            let mut out = Vec::new();
            for major in (11..=floor_major).rev() {
                out.extend(mac_binary_formats((major, 0), arch)
                    .into_iter()
                    .map(|format| format!("macosx_{major}_0_{format}")));
            }
            for minor in (4..=16).rev() {
                let formats = if arch == "x86_64" {
                    mac_binary_formats((10, minor), arch)
                } else {
                    vec!["universal2".to_string()]
                };
                out.extend(formats
                    .into_iter()
                    .map(|format| format!("macosx_10_{minor}_{format}")));
            }
            Ok(out)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TagTriple {
    pub interpreter: String,
    pub abi: String,
    pub platform: String,
}

impl TagTriple {
    pub fn as_string(&self) -> String {
        format!("{}-{}-{}", self.interpreter, self.abi, self.platform)
    }
}

pub fn ordered_target_tags(target: &Target, linux_repair_policy: &str) -> Result<Vec<TagTriple>> {
    let release = target.python_version.release();
    if release.len() < 2 {
        return Err(Error::incompatible(
            format!("target Python version has no minor release: {}", target.python_version),
            Stage::Build,
        ));
    }
    let (major, minor) = (release[0], release[1]);
    let platforms = target_platform_tags(target, linux_repair_policy)?;
    let py_xy = format!("cp{major}{minor}");
    let mut out: Vec<TagTriple> = Vec::new();
    let mut push = |interpreter: &str, abi: &str, platform: &str| {
        let t = TagTriple {
            interpreter: interpreter.to_string(),
            abi: abi.to_string(),
            platform: platform.to_string(),
        };
        if !out.contains(&t) {
            out.push(t);
        }
    };
    for plat in &platforms {
        push(&py_xy, &py_xy, plat);
    }
    for plat in &platforms {
        push(&py_xy, "abi3", plat);
    }
    for plat in &platforms {
        push(&py_xy, "none", plat);
    }
    for older in (2..minor).rev() {
        let interpreter = format!("cp{major}{older}");
        for plat in &platforms {
            push(&interpreter, "abi3", plat);
        }
    }
    for version in py_interpreter_range(major, minor) {
        for plat in &platforms {
            push(&version, "none", plat);
        }
    }
    push(&py_xy, "none", "any");
    for version in py_interpreter_range(major, minor) {
        push(&version, "none", "any");
    }
    Ok(out)
}

fn py_interpreter_range(major: u64, minor: u64) -> Vec<String> {
    let mut out = vec![format!("py{major}{minor}"), format!("py{major}")];
    for older in (0..minor).rev() {
        out.push(format!("py{major}{older}"));
    }
    out
}

pub fn ordered_tag_strings(target: &Target, linux_repair_policy: &str) -> Result<Vec<String>> {
    Ok(ordered_target_tags(target, linux_repair_policy)?
        .iter()
        .map(|t| t.as_string())
        .collect())
}

pub fn tag_preference(target: &Target, linux_repair_policy: &str) -> Result<FxHashMap<String, usize>> {
    let mut map = FxHashMap::default();
    for (i, t) in ordered_target_tags(target, linux_repair_policy)?.iter().enumerate() {
        map.entry(t.as_string()).or_insert(i);
    }
    Ok(map)
}

pub fn wheel_tag_triples(filename: &str) -> Result<Vec<TagTriple>> {
    let stem = filename
        .strip_suffix(".whl")
        .ok_or_else(|| Error::configuration("wheel filename must end in .whl"))?;
    let parts: Vec<&str> = stem.split('-').collect();
    let (py, abi, plat) = match parts.len() {
        5 => (parts[2], parts[3], parts[4]),
        6 => (parts[3], parts[4], parts[5]),
        _ => return Err(Error::configuration("invalid wheel filename")),
    };
    let mut out = Vec::new();
    for p in py.split('.') {
        for a in abi.split('.') {
            for pl in plat.split('.') {
                if p.is_empty() || a.is_empty() || pl.is_empty() {
                    return Err(Error::configuration("invalid wheel tag"));
                }
                out.push(TagTriple {
                    interpreter: p.to_string(),
                    abi: a.to_string(),
                    platform: pl.to_string(),
                });
            }
        }
    }
    Ok(out)
}

pub fn wheel_rank(filename: &str, preference: &FxHashMap<String, usize>) -> Option<usize> {
    let triples = wheel_tag_triples(filename).ok()?;
    triples
        .iter()
        .filter_map(|t| preference.get(&t.as_string()).copied())
        .min()
}

#[derive(Debug)]
pub struct NativeFormatError(pub String);

impl std::fmt::Display for NativeFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for NativeFormatError {}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn u16be(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn u32be(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn native_architectures_from_bytes(content: &[u8]) -> std::result::Result<FxHashSet<String>, NativeFormatError> {
    let err = |m: &str| NativeFormatError(m.to_string());
    if content.starts_with(b"\x7fELF") {
        if content.len() < 16 {
            return Err(err("truncated ELF identification"));
        }
        let class = content[4];
        if class != 1 && class != 2 {
            return Err(err("invalid ELF class"));
        }
        let little = match content[5] {
            1 => true,
            2 => false,
            _ => return Err(err("invalid ELF byte order")),
        };
        if content[6] != 1 {
            return Err(err("invalid ELF version"));
        }
        let header_size: usize = if class == 1 { 52 } else { 64 };
        if content.len() < header_size {
            return Err(err("truncated ELF header"));
        }
        let u16 = if little { u16le } else { u16be };
        let u32 = if little { u32le } else { u32be };
        let machine = u16(&content[18..20]);
        let version = u32(&content[20..24]);
        let declared: u16 = if class == 1 { u16(&content[40..42]) } else { u16(&content[52..54]) };
        if version != 1 || declared as usize != header_size {
            return Err(err("invalid ELF header"));
        }
        let arch = match machine {
            3 => "x86",
            62 => "x86_64",
            183 => "aarch64",
            40 => "arm",
            _ => return Ok([format!("elf-{machine}")].into_iter().collect()),
        };
        return Ok([arch.to_string()].into_iter().collect());
    }
    let macho: Option<(bool, usize)> = match &content.get(..4) {
        Some(b"\xfe\xed\xfa\xce") => Some((false, 28)),
        Some(b"\xce\xfa\xed\xfe") => Some((true, 28)),
        Some(b"\xfe\xed\xfa\xcf") => Some((false, 32)),
        Some(b"\xcf\xfa\xed\xfe") => Some((true, 32)),
        _ => None,
    };
    if let Some((little, header_size)) = macho {
        if content.len() < header_size {
            return Err(err("truncated Mach-O header"));
        }
        let cpu = if little { u32le(&content[4..8]) } else { u32be(&content[4..8]) };
        let arch = match cpu {
            7 => "x86",
            0x01000007 => "x86_64",
            12 => "arm",
            0x0100000C => "arm64",
            _ => return Ok([format!("mach-o-{cpu}")].into_iter().collect()),
        };
        return Ok([arch.to_string()].into_iter().collect());
    }
    let fat: Option<(bool, usize)> = match &content.get(..4) {
        Some(b"\xca\xfe\xba\xbe") => Some((false, 20)),
        Some(b"\xbe\xba\xfe\xca") => Some((true, 20)),
        Some(b"\xca\xfe\xba\xbf") => Some((false, 32)),
        Some(b"\xbf\xba\xfe\xca") => Some((true, 32)),
        _ => None,
    };
    if let Some((little, size)) = fat {
        if content.len() < 8 {
            return Err(err("truncated universal Mach-O header"));
        }
        let count = if little { u32le(&content[4..8]) } else { u32be(&content[4..8]) };
        if count == 0 || count > 64 {
            return Err(err("invalid universal Mach-O architecture count"));
        }
        let rec = size;
        if content.len() < 8 + count as usize * rec {
            return Err(err("truncated universal Mach-O header"));
        }
        let mut result = FxHashSet::default();
        for i in 0..count as usize {
            let off = 8 + i * rec;
            let cpu = if little { u32le(&content[off..off + 4]) } else { u32be(&content[off..off + 4]) };
            let arch = match cpu {
                7 => "x86",
                0x01000007 => "x86_64",
                12 => "arm",
                0x0100000C => "arm64",
                _ => &format!("mach-o-{cpu}"),
            };
            result.insert(arch.to_string());
        }
        return Ok(result);
    }
    if content.starts_with(b"MZ") {
        if content.len() < 64 {
            return Err(err("truncated PE DOS header"));
        }
        let offset = u32le(&content[60..64]) as usize;
        if offset < 64
            || offset + 6 > content.len()
            || &content[offset..offset + 4] != b"PE\x00\x00"
        {
            return Err(err("invalid PE header"));
        }
        let machine = u16le(&content[offset + 4..offset + 6]);
        let arch = match machine {
            0x014C => "x86",
            0x8664 => "x86_64",
            0xAA64 => "arm64",
            _ => return Ok([format!("pe-{machine}")].into_iter().collect()),
        };
        return Ok([arch.to_string()].into_iter().collect());
    }
    Ok(FxHashSet::default())
}

pub fn expected_architectures(filename: &str) -> Result<FxHashSet<String>> {
    let stem = filename
        .strip_suffix(".whl")
        .ok_or_else(|| Error::configuration("wheel filename must end in .whl"))?;
    let parts: Vec<&str> = stem.split('-').collect();
    let plat_group = match parts.len() {
        5 => parts[4],
        6 => parts[5],
        _ => return Err(Error::configuration("invalid wheel filename")),
    };
    let mut result = FxHashSet::default();
    for tag in plat_group.split('.') {
        if tag == "any" {
            continue;
        }
        let mut matched = None;
        for (name, arch) in [
            ("x86_64", "x86_64"),
            ("aarch64", "aarch64"),
            ("arm64", "arm64"),
            ("universal2", "universal2"),
            ("intel", "x86_64"),
        ] {
            if tag.ends_with(name) {
                matched = Some(arch);
                break;
            }
        }
        match matched {
            Some("universal2") => {
                result.insert("x86_64".to_string());
                result.insert("arm64".to_string());
            }
            Some(a) => {
                result.insert(a.to_string());
            }
            None => {
                return Err(Error::integrity(
                    format!("cannot determine architecture from wheel tag {tag}"),
                    Stage::Build,
                ));
            }
        }
    }
    Ok(result)
}

pub fn python_binary() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_string())
}

pub fn verify_repair_tool(target: &Target, env: &FxHashMap<String, String>) -> Result<String> {
    let family = family_of(target.platform.as_str()).map_err(|e| {
        Error::new(Kind::Incompatible, e.detail, Stage::Repair)
    })?;
    let (distribution, module, options, prerequisites) = match family {
        Family::Linux | Family::Musllinux => (
            "auditwheel",
            "auditwheel",
            vec!["--plat", "--wheel-dir"],
            vec!["patchelf", "readelf"],
        ),
        Family::Macos => (
            "delocate",
            "delocate.cmd.delocate_wheel",
            vec!["--wheel-dir"],
            vec!["otool", "install_name_tool"],
        ),
    };
    let installed = installed_distribution(distribution).ok_or_else(|| {
        Error::dependency(
            format!("{distribution} is unavailable; provision it with deploy/provision-mypypi-repair.sh"),
            Stage::Repair,
        )
    })?;
    for exe in &prerequisites {
        if path_lookup(exe).is_none() {
            return Err(Error::dependency(
                format!("{exe} is required by {distribution} on {}", target.platform),
                Stage::Repair,
            ));
        }
    }
    let help_cmd: Vec<String> = if family != Family::Macos {
        vec![
            python_binary(),
            "-m".to_string(),
            module.to_string(),
            "repair".to_string(),
            "--help".to_string(),
        ]
    } else {
        vec![
            python_binary(),
            "-m".to_string(),
            module.to_string(),
            "--help".to_string(),
        ]
    };
    let out = crate::process_policy::run_capture(&help_cmd, None, env, None, 30).map_err(|e| {
        Error::dependency(
            format!("could not verify {distribution} {installed} repair options: {e}"),
            Stage::Repair,
        )
    })?;
    if !out.success() {
        return Err(Error::dependency(
            format!("could not verify {distribution} {installed} repair options"),
            Stage::Repair,
        ));
    }
    let help = format!("{}\n{}", out.stdout, out.stderr);
    let missing: Vec<&str> = options.iter().filter(|o| !help.contains(**o)).copied().collect();
    if !missing.is_empty() {
        return Err(Error::dependency(
            format!("{distribution} {installed} lacks required repair options: {}", missing.join(", ")),
            Stage::Repair,
        ));
    }
    Ok(format!("{distribution} {installed} ({}, {})", target.platform, arch_of(target.platform.as_str())))
}

fn installed_distribution(name: &str) -> Option<String> {
    let out = std::process::Command::new(python_binary())
        .args([
            "-c",
            &format!("import importlib.metadata as m; print(m.version({name:?}))"),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

pub fn path_lookup(exe: &str) -> Option<std::path::PathBuf> {
    if exe.contains('/') {
        let p = std::path::PathBuf::from(exe);
        if p.is_file() {
            return Some(p);
        }
        return None;
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let cand = dir.join(exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub fn repair_command(target: &Target, wheel: &std::path::Path, output: &std::path::Path, linux_repair_policy: &str) -> Result<Vec<String>> {
    match family_of(target.platform.as_str())? {
        Family::Linux | Family::Musllinux => {
            let arch = arch_of(target.platform.as_str());
            let policy = repair_policy(target, linux_repair_policy);
            Ok(vec![
                python_binary(),
                "-mauditwheel".to_string(),
                "repair".to_string(),
                format!("--plat={policy}_{arch}"),
                format!("--wheel-dir={}", output.display()),
                format!("{}", wheel.display()),
            ])
        }
        Family::Macos => Ok(vec![
            python_binary(),
            "-mdelocate.cmd.delocate_wheel".to_string(),
            format!("{}", wheel.display()),
            "--wheel-dir".to_string(),
            format!("{}", output.display()),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn target(platform: &str) -> Target {
        Target::new("3.15", platform).unwrap()
    }

    #[test]
    fn rejects_bad_policy() {
        let t = target("linux-x86_64");
        assert!(target_platform_tags(&t, "musllinux_1_2").is_err());
        assert!(target_platform_tags(&t, "manylinux_2_4").is_err());
        assert!(target_platform_tags(&t, "manylinux_2_5").is_ok());
    }

    #[test]
    fn sniffs_elf_and_macho() {
        let mut elf = vec![0x7fu8, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        elf.extend_from_slice(&[2, 0]);
        elf.extend_from_slice(&[62, 0]);
        elf.extend_from_slice(&[1, 0, 0, 0]);
        while elf.len() < 64 {
            elf.push(0);
        }
        elf[52..54].copy_from_slice(&[64, 0]);
        let arches = native_architectures_from_bytes(&elf).unwrap();
        assert!(arches.contains("x86_64"));
        let macho = [0xcfu8, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01]
            .into_iter()
            .chain(std::iter::repeat_n(0u8, 24))
            .collect::<Vec<_>>();
        let arches = native_architectures_from_bytes(&macho).unwrap();
        assert!(arches.contains("arm64"));
        assert!(native_architectures_from_bytes(b"plain text").unwrap().is_empty());
    }

    #[test]
    fn derives_expected_architectures() {
        let a = expected_architectures("foo-1.0-py3-none-any.whl").unwrap();
        assert!(a.is_empty());
        let a = expected_architectures("foo-1.0-cp315-cp315-manylinux_2_28_x86_64.whl").unwrap();
        assert!(a.contains("x86_64"));
    }

    fn elf_with_interpreter(interpreter: Option<&str>) -> Vec<u8> {
        let mut elf = vec![0u8; 64 + 56 * 2];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[6] = 1;
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&2u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1u32.to_le_bytes());
        if let Some(path) = interpreter {
            let offset = elf.len();
            let mut bytes = path.as_bytes().to_vec();
            bytes.push(0);
            elf[120..124].copy_from_slice(&3u32.to_le_bytes());
            elf[128..136].copy_from_slice(&(offset as u64).to_le_bytes());
            elf[152..160].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
            elf.extend_from_slice(&bytes);
        }
        elf
    }

    #[test]
    fn native_platform_follows_the_host_libc() {
        for (arch, glibc, musl) in [
            ("x86_64", "linux-x86_64", "linux-musl-x86_64"),
            ("aarch64", "linux-aarch64", "linux-musl-aarch64"),
        ] {
            assert_eq!(platform_name("linux", arch, LinuxLibc::Glibc).as_deref(), Some(glibc));
            assert_eq!(platform_name("linux", arch, LinuxLibc::Musl).as_deref(), Some(musl));
        }
        assert_eq!(platform_name("macos", "aarch64", LinuxLibc::Musl).as_deref(), Some("macos-arm64"));
        assert_eq!(platform_name("linux", "riscv64", LinuxLibc::Musl), None);
    }

    #[test]
    fn libc_comes_from_the_program_interpreter() {
        let musl = elf_with_interpreter(Some("/lib/ld-musl-x86_64.so.1"));
        assert_eq!(libc_of_executable(&musl), Some(LinuxLibc::Musl));
        let glibc = elf_with_interpreter(Some("/lib64/ld-linux-x86-64.so.2"));
        assert_eq!(libc_of_executable(&glibc), Some(LinuxLibc::Glibc));
        assert_eq!(libc_of_executable(&elf_with_interpreter(None)), None);
        assert_eq!(libc_of_executable(b"#!/bin/sh\nexec python\n"), None);
        assert_eq!(libc_of_executable(&musl[..100]), None);
    }

    #[test]
    fn local_platform_matches_the_python_loader_on_this_host() {
        let python = path_lookup(&python_binary()).expect("python3 on PATH");
        let content = std::fs::read(python).unwrap();
        let expected = match libc_of_executable(&content) {
            Some(LinuxLibc::Musl) => format!("linux-musl-{}", std::env::consts::ARCH),
            _ => format!("linux-{}", std::env::consts::ARCH),
        };
        if cfg!(target_os = "linux") {
            assert_eq!(current_platform().as_deref(), Some(expected.as_str()));
        }
    }

    #[test]
    fn musl_targets_accept_only_musllinux_tags_up_to_1_2() {
        for (platform, arch) in [("linux-musl-x86_64", "x86_64"), ("linux-musl-aarch64", "aarch64")] {
            let t = target(platform);
            let tags = target_platform_tags(&t, "manylinux_2_28").unwrap();
            assert_eq!(
                tags,
                vec![
                    format!("musllinux_1_2_{arch}"),
                    format!("musllinux_1_1_{arch}"),
                    format!("musllinux_1_0_{arch}"),
                ]
            );
            let ordered = ordered_tag_strings(&t, "manylinux_2_28").unwrap();
            assert_eq!(ordered[0], format!("cp315-cp315-musllinux_1_2_{arch}"));
            assert!(ordered.iter().all(|tag| !tag.contains("manylinux")));
            assert!(!ordered.iter().any(|tag| tag.contains("musllinux_1_3")));
        }
        let glibc = ordered_tag_strings(&target("linux-x86_64"), "manylinux_2_28").unwrap();
        assert!(glibc.iter().all(|tag| !tag.contains("musllinux")));
    }

    #[test]
    fn musl_targets_repair_to_musllinux_whatever_the_manylinux_policy() {
        let t = target("linux-musl-x86_64");
        let command = repair_command(&t, Path::new("w.whl"), Path::new("out"), "manylinux_2_28").unwrap();
        assert!(command.contains(&"--plat=musllinux_1_2_x86_64".to_string()), "{command:?}");
        let glibc = repair_command(&target("linux-aarch64"), Path::new("w.whl"), Path::new("out"), "manylinux_2_28").unwrap();
        assert!(glibc.contains(&"--plat=manylinux_2_28_aarch64".to_string()), "{glibc:?}");
        assert_eq!(repair_policy(&t, "manylinux_2_28"), MUSLLINUX_POLICY);
        assert_eq!(repair_policy(&target("linux-x86_64"), "manylinux_2_28"), "manylinux_2_28");
    }

    #[test]
    fn derives_expected_architectures_for_musllinux() {
        let a = expected_architectures("foo-1.0-cp315-cp315-musllinux_1_2_aarch64.whl").unwrap();
        assert!(a.contains("aarch64") && a.len() == 1);
    }
}
