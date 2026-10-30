use crate::error::{Error, Result};
use crate::fsync::sync_dir;
use crate::hash::{stream_file_hash, HashAlg};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static BLOB_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn sync_immutable_file(src: &Path, dst: &Path, expected_hex: &str, alg: HashAlg) -> Result<()> {
    if !src.exists() {
        return Err(Error::Integrity(format!(
            "missing blob {}",
            src.display()
        )));
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

    if dst.exists() {
        let dst_hash = stream_file_hash(dst, alg)?;
        if dst_hash == expected_hex {
            return Ok(());
        }
        return Err(Error::Integrity(format!(
            "served file mismatch for {}: destination is stale or replaced",
            dst.display()
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
            Error::Integrity(format!("served file changed during install: {}", dst.display()))
        })?;
        let installed = stream_file_hash(dst, alg)?;
        if installed != expected_hex {
            return Err(Error::Integrity(format!(
                "served file changed during install: {}",
                dst.display()
            )));
        }
    } else {
        let dst_hash = stream_file_hash(dst, alg)?;
        if dst_hash != expected_hex {
            return Err(Error::Integrity(format!(
                "served file mismatch for {}: destination is stale or replaced",
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
        assert_eq!(fs::read(&dst).unwrap(), b"other");
    }

    #[test]
    fn supports_sha512() {
        let dir = tempdir().unwrap();
        let (src, hex) = fixture(dir.path(), "a.tgz", b"tarball", HashAlg::Sha512);
        let dst = dir.path().join("out/a.tgz");
        sync_immutable_file(&src, &dst, &hex, HashAlg::Sha512).unwrap();
        assert_eq!(hex.len(), 128);
    }
}
