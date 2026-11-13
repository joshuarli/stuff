use crate::error::{Error, Result};
use crate::glob::glob_match;
use crate::manifest::{ArchiveFormat, Artifact};
use std::io::Read;

const MAX_MEMBERS: usize = 100_000;
const MAX_DECOMPRESSED: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemberKind {
    File,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub path: String,
    pub kind: MemberKind,
    pub mode: u32,
    pub data: Vec<u8>,
}

pub(crate) fn extract(artifact: &Artifact, bytes: &[u8]) -> Result<Vec<Member>> {
    let raw = match artifact.format()? {
        ArchiveFormat::Raw => vec![raw_member(artifact, bytes)?],
        ArchiveFormat::TarGzip => tar_members(flate2::read::GzDecoder::new(bytes), artifact)?,
        ArchiveFormat::TarXz => {
            let mut decompressed = Vec::new();
            let mut input = std::io::Cursor::new(bytes);
            lzma_rs::xz_decompress(&mut input, &mut decompressed).map_err(|error| {
                Error::archive(format!("{}: could not decompress xz: {error}", artifact.id))
            })?;
            enforce_cap(artifact, decompressed.len() as u64)?;
            tar_members(std::io::Cursor::new(decompressed), artifact)?
        }
        ArchiveFormat::Zip => zip_members(bytes, artifact)?,
    };
    assemble(artifact, raw)
}

fn raw_member(artifact: &Artifact, bytes: &[u8]) -> Result<Member> {
    if artifact.strip_components > 0 {
        return Err(Error::manifest(format!(
            "{}: strip_components applies to archives, not to raw files",
            artifact.id
        )));
    }
    let name = artifact
        .url
        .split(['?', '#'])
        .next()
        .and_then(|value| value.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::archive(format!("{}: url has no file name", artifact.id)))?;
    let path = normalize_member_path(name)
        .ok_or_else(|| Error::archive(format!("{}: url file name '{name}' is not usable", artifact.id)))?;
    Ok(Member {
        path,
        kind: MemberKind::File,
        mode: 0o100644,
        data: bytes.to_vec(),
    })
}

fn enforce_cap(artifact: &Artifact, bytes: u64) -> Result<()> {
    if bytes > MAX_DECOMPRESSED {
        return Err(Error::archive(format!(
            "{}: decompressed content exceeds {MAX_DECOMPRESSED} bytes",
            artifact.id
        )));
    }
    Ok(())
}

fn tar_members(reader: impl Read, artifact: &Artifact) -> Result<Vec<Member>> {
    let mut archive = tar::Archive::new(reader);
    let mut members = Vec::new();
    let mut total = 0u64;
    let entries = archive
        .entries()
        .map_err(|error| Error::archive(format!("{}: could not read tar archive: {error}", artifact.id)))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| Error::archive(format!("{}: could not read tar entry: {error}", artifact.id)))?;
        if members.len() >= MAX_MEMBERS {
            return Err(Error::archive(format!("{}: archive exceeds {MAX_MEMBERS} members", artifact.id)));
        }
        let header = entry.header().clone();
        let path_bytes = entry.path_bytes();
        let raw_name = String::from_utf8(path_bytes.into_owned())
            .map_err(|_| Error::archive(format!("{}: member path is not valid UTF-8", artifact.id)))?;
        match header.entry_type() {
            tar::EntryType::Directory => continue,
            tar::EntryType::GNULongName | tar::EntryType::GNULongLink | tar::EntryType::XHeader | tar::EntryType::XGlobalHeader => continue,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let mode = 0o100000 | (header.mode().unwrap_or(0o644) & 0o777);
                let mut data = Vec::new();
                entry
                    .read_to_end(&mut data)
                    .map_err(|error| Error::archive(format!("{}: could not read member {raw_name}: {error}", artifact.id)))?;
                total += data.len() as u64;
                enforce_cap(artifact, total)?;
                members.push(Member {
                    path: raw_name,
                    kind: MemberKind::File,
                    mode,
                    data,
                });
            }
            tar::EntryType::Symlink => {
                let target = header
                    .link_name_bytes()
                    .map(|value| String::from_utf8_lossy(&value).into_owned())
                    .ok_or_else(|| Error::archive(format!("{}: member {raw_name} has no link target", artifact.id)))?;
                members.push(Member {
                    path: raw_name,
                    kind: MemberKind::Symlink,
                    mode: 0o120000 | 0o777,
                    data: target.into_bytes(),
                });
            }
            tar::EntryType::Link => {
                return Err(Error::archive(format!(
                    "{}: member {raw_name} is a hard link; hard links are not supported",
                    artifact.id
                )));
            }
            other => {
                return Err(Error::archive(format!(
                    "{}: member {raw_name} has unsupported entry type {}",
                    artifact.id,
                    other.as_byte()
                )));
            }
        }
    }
    Ok(members)
}

fn zip_members(bytes: &[u8], artifact: &Artifact) -> Result<Vec<Member>> {
    let archive = rawzip::ZipArchive::from_slice(bytes)
        .map_err(|error| Error::archive(format!("{}: could not read zip archive: {error}", artifact.id)))?
        .into_cursor_archive();
    let mut buffer = vec![0u8; rawzip::RECOMMENDED_BUFFER_SIZE];
    let mut entries_meta = Vec::new();
    {
        let mut entries = archive.entries(&mut buffer);
        while let Some(entry) = entries
            .next_entry()
            .map_err(|error| Error::archive(format!("{}: could not read zip entry: {error}", artifact.id)))?
        {
            if entry.is_dir() {
                continue;
            }
            entries_meta.push((
                entry.wayfinder(),
                String::from_utf8(entry.file_path().as_bytes().to_vec()).map_err(|_| {
                    Error::archive(format!("{}: member path is not valid UTF-8", artifact.id))
                })?,
                entry.compression_method(),
                entry.mode().value(),
            ));
            if entries_meta.len() >= MAX_MEMBERS {
                return Err(Error::archive(format!("{}: archive exceeds {MAX_MEMBERS} members", artifact.id)));
            }
        }
    }
    let mut members = Vec::with_capacity(entries_meta.len());
    let mut total = 0u64;
    for (wayfinder, name, method, mode) in entries_meta {
        let entry = archive
            .get_entry(wayfinder)
            .map_err(|error| Error::archive(format!("{}: could not read member {name}: {error}", artifact.id)))?;
        let raw = entry.reader();
        let inner: Box<dyn Read> = match method {
            rawzip::CompressionMethod::STORE => Box::new(raw),
            rawzip::CompressionMethod::DEFLATE => Box::new(flate2::read::DeflateDecoder::new(raw)),
            other => {
                return Err(Error::archive(format!(
                    "{}: member {name} uses unsupported compression method {other:?}",
                    artifact.id
                )));
            }
        };
        let mut data = Vec::new();
        entry
            .verifying_reader(inner.take(MAX_DECOMPRESSED + 1))
            .read_to_end(&mut data)
            .map_err(|error| Error::archive(format!("{}: could not read member {name}: {error}", artifact.id)))?;
        total += data.len() as u64;
        if total > MAX_DECOMPRESSED {
            return Err(Error::archive(format!(
                "{}: archive exceeds {MAX_DECOMPRESSED} decompressed bytes",
                artifact.id
            )));
        }
        let is_symlink = mode & 0o170000 == 0o120000;
        let kind = if is_symlink { MemberKind::Symlink } else { MemberKind::File };
        let normalized_mode = if is_symlink {
            0o120000 | 0o777
        } else if mode & 0o170000 == 0o100000 {
            mode
        } else {
            0o100000 | (mode & 0o777)
        };
        members.push(Member {
            path: name,
            kind,
            mode: normalized_mode,
            data,
        });
    }
    Ok(members)
}

fn assemble(artifact: &Artifact, raw: Vec<Member>) -> Result<Vec<Member>> {
    let strip = artifact.strip_components as usize;
    let mut seen = Vec::with_capacity(raw.len());
    for member in raw {
        let path = normalize_member_path(&member.path).ok_or_else(|| {
            Error::archive(format!(
                "{}: member path '{}' is not safe",
                artifact.id, member.path
            ))
        })?;
        if strip == 0 {
            // nothing to drop
        } else {
            let components: Vec<&str> = path.split('/').collect();
            if components.len() <= strip {
                continue;
            }
            let stripped = components[strip..].join("/");
            seen.push(Member { path: stripped, ..member });
            continue;
        }
        seen.push(Member { path, ..member });
    }
    let mut paths = std::collections::HashSet::with_capacity(seen.len());
    for member in &seen {
        if !paths.insert(member.path.clone()) {
            return Err(Error::archive(format!(
                "{}: duplicate member path '{}' after stripping",
                artifact.id, member.path
            )));
        }
    }
    if seen.is_empty() {
        return Err(Error::archive(format!(
            "{}: archive contains no file members after stripping {} components",
            artifact.id, strip
        )));
    }
    for member in &seen {
        if member.kind == MemberKind::Symlink {
            let target = String::from_utf8_lossy(&member.data);
            validate_symlink_target(&member.path, &target)
                .map_err(|error| Error::archive(format!("{}: {error}", artifact.id)))?;
        }
    }
    let kept: Vec<Member> = match &artifact.keep {
        None => seen,
        Some(patterns) => seen
            .into_iter()
            .filter(|member| patterns.iter().any(|pattern| glob_match(pattern, &member.path)))
            .collect(),
    };
    if kept.is_empty() {
        return Err(Error::archive(format!(
            "{}: no files matched the keep list",
            artifact.id
        )));
    }
    let mut kept = kept;
    kept.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(kept)
}

fn normalize_member_path(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.contains('\\') || raw.starts_with('/') || raw.starts_with('~') {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return None;
    }
    let mut stack: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        return None;
    }
    Some(stack.join("/"))
}

fn validate_symlink_target(member: &str, target: &str) -> Result<()> {
    if target.is_empty()
        || target.contains('\\')
        || target.starts_with('/')
        || target.starts_with('~')
    {
        return Err(Error::integrity(format!(
            "member {member} has an unsafe link target '{target}'"
        )));
    }
    let bytes = target.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(Error::integrity(format!(
            "member {member} has an unsafe link target '{target}'"
        )));
    }
    let mut depth = member.matches('/').count();
    for part in target.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    Error::integrity(format!(
                        "member {member} has a link target escaping the archive root: '{target}'"
                    ))
                })?;
            }
            _ => depth += 1,
        }
    }
    Ok(())
}
