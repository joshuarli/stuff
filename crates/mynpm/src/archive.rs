#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::inventory::{valid_package_name, valid_version};
use rustc_hash::FxHashSet;
use std::fs;
use std::io::Read;

#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub entry_type: char,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct InspectedPackageArchive {
    pub manifest: deser_value::Value,
    pub entries: Vec<ArchiveEntry>,
    pub compressed_bytes: u64,
    pub unpacked_bytes: u64,
}

fn has_control_character(path: &str) -> bool {
    path.chars().any(|c| c <= '\x1f' || c == '\x7f')
}

pub fn inspect_package_archive(archive_path: &str, expected_name: &str, expected_version: &str) -> Result<InspectedPackageArchive> {
    inspect_archive(archive_path, Some((expected_name, expected_version)))
}

pub fn inspect_unidentified_package_archive(archive_path: &str) -> Result<InspectedPackageArchive> {
    inspect_archive(archive_path, None)
}

fn inspect_archive(archive_path: &str, expected: Option<(&str, &str)>) -> Result<InspectedPackageArchive> {
    let meta = fs::symlink_metadata(archive_path)
        .map_err(|_| Error::Input(format!("could not inspect package archive {archive_path}")))?;
    if !meta.is_file() {
        return Err(Error::Input("package archive is not a regular file".to_string()));
    }
    if meta.len() > crate::limits::TARBALL_BYTES {
        return Err(Error::Input(format!(
            "package archive exceeds the {} compressed size limit",
            crate::limits::format_bytes(crate::limits::TARBALL_BYTES)
        )));
    }

    let file = fs::File::open(archive_path)
        .map_err(|_| Error::Input(format!("could not inspect package archive {archive_path}")))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut seen: FxHashSet<String> = FxHashSet::default();
    let mut unpacked_bytes: u64 = 0;
    let mut root_directory: Option<String> = None;
    let mut manifest_bytes: Option<Vec<u8>> = None;

    let raw_entries = archive
        .entries()
        .map_err(|_| Error::Input("package archive could not be inspected by tar".to_string()))?;
    for raw in raw_entries {
        let mut entry = raw
            .map_err(|_| Error::Input("package archive could not be inspected by tar".to_string()))?;
        let kind = entry.header().entry_type();
        let entry_type = match kind {
            tar::EntryType::Regular => '-',
            tar::EntryType::Directory => 'd',
            _ => {
                let raw_path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
                return Err(Error::Input(format!(
                    "package archive contains an unsupported entry type at {raw_path}"
                )));
            }
        };
        let bytes_path = entry.path_bytes();
        let path_str = std::str::from_utf8(&bytes_path)
            .map_err(|_| Error::Input("package archive contains a non-UTF-8 path".to_string()))?
            .to_string();
        if path_str.len() > 4096 {
            return Err(Error::Input("package archive entry path exceeds the supported path limit".to_string()));
        }
        let mut path = path_str;
        if path.ends_with('/') {
            path.pop();
        }
        if path.is_empty() || path != path.trim() || path.contains('\\') || path.contains('"') || path.contains('\'') || has_control_character(&path) {
            return Err(Error::Input("package archive contains an unsupported entry path".to_string()));
        }
        let separator = path.find('/');
        let root = match separator {
            Some(i) => path[..i].to_string(),
            None => path.clone(),
        };
        if root == "." || root == ".." {
            return Err(Error::Input(format!("package archive entry is outside package/: {path}")));
        }
        if let Some(ref prev) = root_directory
            && *prev != root {
                return Err(Error::Input(format!("package archive contains multiple roots: {root}")));
            }
        root_directory = Some(root.clone());
        let normalized = if path == root {
            "package".to_string()
        } else {
            let rest: Vec<&str> = path[root.len() + 1..].split('/').filter(|part| *part != ".").collect();
            if rest.is_empty() { "package".to_string() } else { format!("package/{}", rest.join("/")) }
        };
        if normalized.starts_with('/') || normalized.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            return Err(Error::Input(format!("package archive entry has an unsafe path: {normalized}")));
        }
        let duplicate = !seen.insert(normalized.clone());
        if duplicate {
            let replaces_regular_file = entry_type == '-'
                && normalized != "package/package.json"
                && entries.iter().any(|e| e.path == normalized && e.entry_type == '-');
            if !replaces_regular_file {
                return Err(Error::Input(format!("package archive contains duplicate entry {normalized}")));
            }
            entries.retain(|e| e.path != normalized);
        }
        if normalized == "package" && entry_type != 'd' {
            return Err(Error::Input("package archive root must be a directory".to_string()));
        }
        let size = entry.header().size().unwrap_or(0);
        if entry_type == '-' {
            unpacked_bytes = unpacked_bytes.checked_add(size).ok_or_else(|| Error::Input(format!(
                "package archive exceeds the {} unpacked size limit",
                crate::limits::format_bytes(crate::limits::UNPACKED_BYTES)
            )))?;
            if unpacked_bytes > crate::limits::UNPACKED_BYTES {
                return Err(Error::Input(format!(
                    "package archive exceeds the {} unpacked size limit",
                    crate::limits::format_bytes(crate::limits::UNPACKED_BYTES)
                )));
            }
        }
        if normalized == "package/package.json" && entry_type == '-' {
            if size > crate::limits::METADATA_BYTES {
                return Err(Error::Input(format!(
                    "package/package.json exceeds the {} metadata limit",
                    crate::limits::format_bytes(crate::limits::METADATA_BYTES)
                )));
            }
            let mut contents = Vec::with_capacity(size.min(crate::limits::METADATA_BYTES) as usize);
            entry
                .read_to_end(&mut contents)
                .map_err(|_| Error::Input("package archive manifest could not be read".to_string()))?;
            manifest_bytes = Some(contents);
        }
        entries.push(ArchiveEntry { entry_type, size, path: normalized });
        if entries.len() > crate::limits::ARCHIVE_ENTRIES {
            return Err(Error::Input("package archive exceeds the 100,000 entry limit".to_string()));
        }
    }

    if entries.is_empty() {
        return Err(Error::Input("package archive is empty".to_string()));
    }
    let root_directory = root_directory.ok_or_else(|| Error::Input("package archive is empty".to_string()))?;
    let manifest_count = entries.iter().filter(|e| e.path == "package/package.json" && e.entry_type == '-').count();
    if manifest_count != 1 {
        return Err(Error::Input("package archive must contain exactly one package/package.json file".to_string()));
    }
    let manifest_text = String::from_utf8(manifest_bytes.unwrap_or_default())
        .map_err(|_| Error::Input("package archive package/package.json is not valid JSON".to_string()))?;
    let manifest: deser_value::Value = deser_json::from_str(&manifest_text)
        .map_err(|_| Error::Input("package archive package/package.json is not valid JSON".to_string()))?;
    let mmap = manifest.as_map().ok_or_else(|| Error::Input("package archive package.json must contain an object".to_string()))?;
    let name = mmap.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let version = mmap.get("version").and_then(|v| v.as_str()).unwrap_or("");
    if let Some((expected_name, expected_version)) = expected
        && (name != expected_name || version != expected_version)
    {
        return Err(Error::Input(format!(
            "package archive identity mismatch: expected {expected_name}@{expected_version}, found {name}@{version}"
        )));
    }
    if !valid_package_name(name) || !valid_version(version) {
        return Err(Error::Input("package archive package.json has an invalid identity".to_string()));
    }
    let _ = root_directory;
    Ok(InspectedPackageArchive {
        manifest,
        entries,
        compressed_bytes: meta.len(),
        unpacked_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tarball(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg.tgz").to_string_lossy().to_string();
        let file = fs::File::create(&path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        for (name, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        (dir, path)
    }

    #[test]
    fn inspects_valid_archive() {
        let manifest = br#"{"name":"flatdemo","version":"1.2.3"}"#;
        let (_dir, path) = make_tarball(&[("package/package.json", manifest), ("package/index.js", b"module.exports=1")]);
        let inspected = inspect_package_archive(&path, "flatdemo", "1.2.3").unwrap();
        assert_eq!(inspected.entries.len(), 2);
        assert!(inspected.entries.iter().any(|e| e.path == "package/package.json"));
        assert!(inspected.unpacked_bytes > 0);
    }

    #[test]
    fn rejects_identity_mismatch_and_bad_type() {
        let manifest = br#"{"name":"other","version":"1.2.3"}"#;
        let (_dir, path) = make_tarball(&[("package/package.json", manifest)]);
        let err = inspect_package_archive(&path, "flatdemo", "1.2.3").unwrap_err();
        assert!(format!("{err}").contains("identity mismatch"));
    }

    #[test]
    fn rejects_missing_manifest() {
        let (_dir, path) = make_tarball(&[("package/index.js", b"x")]);
        let err = inspect_package_archive(&path, "flatdemo", "1.2.3").unwrap_err();
        assert!(format!("{err}").contains("exactly one package/package.json"));
    }

    fn tar_entry_raw(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size_str = format!("{:07o}\0", contents.len());
        header[124..132].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..262].copy_from_slice(b"ustar");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|&b| b as u32).sum();
        let chk = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(chk.as_bytes());
        let mut out = header;
        out.extend_from_slice(contents);
        while !out.len().is_multiple_of(512) {
            out.push(0);
        }
        out
    }

    fn make_raw_tarball(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, String) {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg.tgz").to_string_lossy().to_string();
        let file = fs::File::create(&path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        for (name, contents) in entries {
            enc.write_all(&tar_entry_raw(name, contents)).unwrap();
        }
        enc.write_all(&vec![0u8; 1024]).unwrap();
        enc.finish().unwrap();
        (dir, path)
    }

    #[test]
    fn accepts_any_single_root_and_current_directory_segments_like_npm() {
        let manifest = br#"{"name":"@types/node","version":"24.19.2"}"#;
        let (_dir, path) = make_raw_tarball(&[("node v24.19/package.json", manifest), ("node v24.19/./index.d.ts", b"x")]);
        let inspected = inspect_package_archive(&path, "@types/node", "24.19.2").unwrap();
        let paths: Vec<&str> = inspected.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["package/package.json", "package/index.d.ts"]);
    }

    #[test]
    fn a_repeated_regular_file_replaces_the_earlier_entry_like_npm() {
        let manifest = br#"{"name":"agent-base","version":"7.1.4"}"#;
        let (_dir, path) = make_raw_tarball(&[("package/package.json", manifest), ("package/./dist/index.js", b"a"), ("package/dist/index.js", b"b")]);
        let inspected = inspect_package_archive(&path, "agent-base", "7.1.4").unwrap();
        let paths: Vec<&str> = inspected.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["package/package.json", "package/dist/index.js"]);

        let (_dir, path) = make_raw_tarball(&[("package/package.json", manifest), ("package/./package.json", manifest)]);
        assert!(format!("{}", inspect_package_archive(&path, "agent-base", "7.1.4").unwrap_err()).contains("duplicate entry"));
    }

    #[test]
    fn rejects_multiple_roots_and_dot_roots() {
        let manifest = br#"{"name":"flatdemo","version":"1.2.3"}"#;
        let (_dir, path) = make_raw_tarball(&[("package/package.json", manifest), ("other/index.js", b"x")]);
        assert!(format!("{}", inspect_package_archive(&path, "flatdemo", "1.2.3").unwrap_err()).contains("multiple roots"));
        let (_dir, path) = make_raw_tarball(&[("./package.json", manifest)]);
        assert!(format!("{}", inspect_package_archive(&path, "flatdemo", "1.2.3").unwrap_err()).contains("outside package"));
    }

    #[test]
    fn rejects_traversal() {
        let manifest = br#"{"name":"flatdemo","version":"1.2.3"}"#;
        let (_dir, path) = make_raw_tarball(&[("package/package.json", manifest), ("package/../evil", b"x")]);
        let err = inspect_package_archive(&path, "flatdemo", "1.2.3").unwrap_err();
        let message = format!("{err}");
        assert!(message.contains("unsafe path") || message.contains("outside package"), "{message}");
    }
}
