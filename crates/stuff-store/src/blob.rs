use crate::error::{Error, Result};
use crate::fsync::sync_dir;
use crate::hash::{stream_file_hash, HashAlg};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static BLOB_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn sync_immutable_file(src: &Path, dst: &Path, expected_hex: &str, alg: HashAlg) -> Result<()> {
    let src_meta = fs::symlink_metadata(src).map_err(|_| {
        Error::Integrity(format!("missing blob {}", src.display()))
    })?;
    if !src_meta.is_file() || src_meta.file_type().is_symlink() {
        return Err(Error::Integrity(format!(
            "managed path is not a regular file: {}",
            src.display()
        )));
    }

    match fs::symlink_metadata(dst) {
        Ok(dst_meta) => {
            if !dst_meta.is_file() || dst_meta.file_type().is_symlink() {
                return Err(Error::Integrity(format!(
                    "served blob mismatch for {expected_hex}: destination is not a regular file ({})",
                    dst.display()
                )));
            }
            if (dst_meta.dev(), dst_meta.ino()) == (src_meta.dev(), src_meta.ino()) {
                return Ok(());
            }
            if dst_meta.len() == src_meta.len() {
                return Ok(());
            }
            return Err(Error::Integrity(format!(
                "served blob mismatch for {expected_hex}: destination is stale or replaced ({})",
                dst.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error::Operation(format!(
                "could not inspect served blob {}: {e}",
                dst.display()
            )));
        }
    }

    let actual = stream_file_hash(src, alg)?;
    if actual != expected_hex {
        return Err(Error::Integrity(format!(
            "tampered blob {}: expected {}, got {}",
            src.display(),
            expected_hex,
            actual
        )));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::hard_link(src, dst).is_err() {
        let parent = dst.parent().ok_or_else(|| {
            Error::Input(format!("destination has no parent: {}", dst.display()))
        })?;
        let tmp = parent.join(format!(
            ".tmp.install.{}.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            BLOB_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        {
            let mut from = File::open(src)?;
            let mut to = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
            let mut buf = [0u8; 65536];
            loop {
                let n = from.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                to.write_all(&buf[..n])?;
            }
            to.sync_all()?;
        }
        fs::rename(&tmp, dst).map_err(|_| {
            let _ = fs::remove_file(&tmp);
            Error::Integrity(format!(
                "served blob changed during install: {expected_hex} ({})",
                dst.display()
            ))
        })?;
        let installed = stream_file_hash(dst, alg)?;
        if installed != expected_hex {
            return Err(Error::Integrity(format!(
                "served blob changed during install: {expected_hex} ({})",
                dst.display()
            )));
        }
    } else {
        let dst_hash = stream_file_hash(dst, alg)?;
        if dst_hash != expected_hex {
            return Err(Error::Integrity(format!(
                "served blob mismatch for {expected_hex}: destination is stale or replaced ({})",
                dst.display()
            )));
        }
    }
    if let Some(parent) = dst.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn fixture(dir: &Path, name: &str, bytes: &[u8], alg: HashAlg) -> (std::path::PathBuf, String) {
        let p = dir.join(name);
        File::create(&p).unwrap().write_all(bytes).unwrap();
        let hex = stream_file_hash(&p, alg).unwrap();
        (p, hex)
    }

    #[test]
    fn links_or_copies_and_skips_matching_destination() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.bin", b"payload", HashAlg::Sha256);
        let dst = dir.path().join("out/a.bin");
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap();
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"payload");
    }

    #[test]
    fn rejects_tampered_source() {
        let dir = tempdir().unwrap();
        let (src, _) = fixture(dir.path(), "a.bin", b"payload", HashAlg::Sha256);
        let dst = dir.path().join("out/a.bin");
        let err = sync_immutable_file(&src, &dst, &"0".repeat(64), HashAlg::Sha256).unwrap_err();
        assert!(matches!(err, Error::Integrity(_)));
        assert!(!dst.exists());
    }

    #[test]
    fn never_overwrites_mismatched_destination() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.bin", b"payload", HashAlg::Sha256);
        let dst = dir.path().join("out/a.bin");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"other").unwrap();
        let err = sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap_err();
        assert!(matches!(err, Error::Integrity(_)));
        assert!(format!("{err}").contains("served blob mismatch"));
        assert_eq!(fs::read(&dst).unwrap(), b"other");
    }

    #[test]
    fn detects_replaced_served_copy() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.bin", b"payload", HashAlg::Sha256);
        let dst = dir.path().join("out/a.bin");
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap();
        let evil = dir.path().join("evil.bin");
        fs::write(&evil, b"tampered").unwrap();
        fs::rename(&evil, &dst).unwrap();
        let err = sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap_err();
        assert!(format!("{err}").contains("served blob mismatch"));
    }

    #[test]
    fn supports_sha512() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.tgz", b"tarball", HashAlg::Sha512);
        let dst = dir.path().join("out/a.tgz");
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha512).unwrap();
        assert_eq!(hex.len(), 128);
    }

    #[test]
    fn accepts_a_same_size_destination_without_rehashing() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.bin", b"payload", HashAlg::Sha256);
        let dst = dir.path().join("out/a.bin");
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"sevens!").unwrap();
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"sevens!");
    }

    #[test]
    fn a_hard_linked_destination_is_not_rehashed() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.bin", b"payload", HashAlg::Sha256);
        let dst = dir.path().join("out/a.bin");
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap();
        File::options()
            .write(true)
            .open(&dst)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha256).unwrap();
    }
}
