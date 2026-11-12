//! Read access to wheel and sdist zip archives.

use std::io::Read;
use std::path::Path;

use rawzip::{CompressionMethod, FileReader, RECOMMENDED_BUFFER_SIZE, ZipArchive, ZipArchiveEntryWayfinder};

#[derive(Debug)]
pub struct ArchiveError(String);

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ArchiveError {}

fn error(value: impl std::fmt::Display) -> ArchiveError {
    ArchiveError(value.to_string())
}

struct Member {
    name: String,
    wayfinder: ZipArchiveEntryWayfinder,
    method: CompressionMethod,
    mode: u32,
    size: u64,
}

/// A zip archive whose central directory has been read, with members in archive order.
pub struct Archive {
    archive: ZipArchive<FileReader>,
    members: Vec<Member>,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self, ArchiveError> {
        let file = std::fs::File::open(path).map_err(error)?;
        let mut buffer = vec![0u8; RECOMMENDED_BUFFER_SIZE];
        let archive = ZipArchive::from_file(file, &mut buffer).map_err(error)?;
        let mut members = Vec::new();
        {
            let mut entries = archive.entries(&mut buffer);
            while let Some(entry) = entries.next_entry().map_err(error)? {
                let name = std::str::from_utf8(entry.file_path().as_bytes())
                    .map_err(|_| ArchiveError("archive member name is not valid UTF-8".to_string()))?
                    .to_string();
                members.push(Member {
                    name,
                    wayfinder: entry.wayfinder(),
                    method: entry.compression_method(),
                    mode: entry.mode().value(),
                    size: entry.uncompressed_size_hint(),
                });
            }
        }
        Ok(Self { archive, members })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|member| member.name.as_str())
    }

    /// The member's mode with Unix file type bits (`0o170000`); untyped entries read as regular files.
    pub fn mode(&self, name: &str) -> Result<u32, ArchiveError> {
        Ok(self.member(name)?.mode)
    }

    /// The member's uncompressed size as recorded in the central directory.
    pub fn size(&self, name: &str) -> Result<u64, ArchiveError> {
        Ok(self.member(name)?.size)
    }

    /// Reads the whole member, verifying its size and CRC.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, ArchiveError> {
        let mut out = Vec::new();
        self.reader(name)?.read_to_end(&mut out).map_err(error)?;
        Ok(out)
    }

    /// Reads at most `limit` decompressed bytes of the member.
    pub fn read_prefix(&self, name: &str, limit: u64) -> Result<Vec<u8>, ArchiveError> {
        let mut out = Vec::new();
        self.reader(name)?.take(limit).read_to_end(&mut out).map_err(error)?;
        Ok(out)
    }

    fn member(&self, name: &str) -> Result<&Member, ArchiveError> {
        self.members
            .iter()
            .find(|member| member.name == name)
            .ok_or_else(|| ArchiveError(format!("archive has no member {name}")))
    }

    fn reader(&self, name: &str) -> Result<Box<dyn Read + '_>, ArchiveError> {
        let member = self.member(name)?;
        let entry = self.archive.get_entry(member.wayfinder).map_err(error)?;
        let raw = entry.reader();
        match member.method {
            CompressionMethod::STORE => Ok(Box::new(entry.verifying_reader(raw))),
            CompressionMethod::DEFLATE => {
                Ok(Box::new(entry.verifying_reader(flate2::read::DeflateDecoder::new(raw))))
            }
            method => Err(ArchiveError(format!("member {name} uses unsupported compression method {method:?}"))),
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::io::Write;
    use std::path::Path;

    /// Writes deflate-compressed regular files, or symbolic links when `mode` is a symlink mode.
    pub(crate) fn write_archive(path: &Path, members: &[(&str, &[u8], u32)]) {
        let file = std::fs::File::create(path).unwrap();
        let mut archive = rawzip::ZipArchiveWriter::new(file);
        for (name, data, mode) in members {
            let (mut entry, config) = archive
                .new_file(*name)
                .compression_method(rawzip::CompressionMethod::DEFLATE)
                .unix_permissions(*mode)
                .start()
                .unwrap();
            let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
            let mut writer = config.wrap(encoder);
            writer.write_all(data).unwrap();
            let (encoder, descriptor) = writer.finish().unwrap();
            encoder.finish().unwrap();
            entry.finish(descriptor).unwrap();
        }
        archive.finish().unwrap();
    }

    pub(crate) fn write_files(path: &Path, members: &[(&str, &[u8])]) {
        let regular: Vec<(&str, &[u8], u32)> = members.iter().map(|(name, data)| (*name, *data, 0o100644)).collect();
        write_archive(path, &regular);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_members_in_order_with_their_file_types() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.whl");
        testing::write_archive(
            &path,
            &[("pkg/__init__.py", b"x = 1\n", 0o100644), ("pkg/link", b"target", 0o120777)],
        );

        let archive = Archive::open(&path).unwrap();

        assert_eq!(archive.names().collect::<Vec<_>>(), ["pkg/__init__.py", "pkg/link"]);
        assert_eq!(archive.mode("pkg/link").unwrap() & 0o170000, 0o120000);
        assert_eq!(archive.mode("pkg/__init__.py").unwrap() & 0o170000, 0o100000);
        assert_eq!(archive.read("pkg/__init__.py").unwrap(), b"x = 1\n");
        assert_eq!(archive.read_prefix("pkg/__init__.py", 2).unwrap(), b"x ");
        assert_eq!(archive.size("pkg/__init__.py").unwrap(), 6);
        assert!(archive.read("pkg/missing").is_err());
    }

    #[test]
    fn rejects_a_file_that_is_not_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.whl");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(Archive::open(&path).is_err());
    }
}
