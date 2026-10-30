use crate::error::Result;
use sha2::{Digest, Sha256, Sha512};
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlg {
    Sha256,
    Sha512,
}

pub fn stream_file_hash(path: &Path, alg: HashAlg) -> Result<String> {
    let mut f = File::open(path)?;
    let mut buf = [0u8; 65536];
    match alg {
        HashAlg::Sha256 => {
            let mut h = Sha256::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(format!("{:x}", h.finalize()))
        }
        HashAlg::Sha512 => {
            let mut h = Sha512::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(format!("{:x}", h.finalize()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn known_vectors() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("v.bin");
        File::create(&p).unwrap().write_all(b"abc").unwrap();
        assert_eq!(
            stream_file_hash(&p, HashAlg::Sha256).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            stream_file_hash(&p, HashAlg::Sha512).unwrap(),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }
}
