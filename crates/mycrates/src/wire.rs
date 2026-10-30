use crate::error::{Error, Result};
use crate::model::PublishBody;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static WIRE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct TmpCleanup(PathBuf);

impl Drop for TmpCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn spill_path(dir: &Path) -> PathBuf {
    let unique = format!(
        ".upload.{}.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        WIRE_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    dir.join(unique)
}

#[derive(Debug)]
pub struct StagedUpload {
    pub body: PublishBody,
    pub crate_bytes_path: PathBuf,
    pub crate_len: u64,
    pub crate_sha256: String,
    _cleanup: TmpCleanup,
}

impl StagedUpload {
    pub fn open(&self) -> Result<std::fs::File> {
        std::fs::File::open(&self.crate_bytes_path).map_err(Error::from)
    }
}

pub fn decode_publish_payload<R: Read>(
    mut reader: R,
    content_length: u64,
    max_compressed: u64,
    staging_dir: &Path,
) -> Result<StagedUpload> {
    if content_length < 8 {
        return Err(Error::Input("upload payload too short".to_string()));
    }
    if content_length > max_compressed + 10 * 1024 * 1024 {
        return Err(Error::Limit("upload payload exceeds size limit".to_string()));
    }

    fs::create_dir_all(staging_dir)?;

    let mut json_len_buf = [0u8; 4];
    reader
        .read_exact(&mut json_len_buf)
        .map_err(|e| Error::Input(format!("failed to read json length: {e}")))?;
    let json_len = u32::from_le_bytes(json_len_buf) as u64;

    if json_len == 0 {
        return Err(Error::Input("empty json metadata".to_string()));
    }
    if json_len > 10 * 1024 * 1024 || json_len > content_length - 8 {
        return Err(Error::Limit("json metadata length invalid or too large".to_string()));
    }

    let mut json_buf = vec![0u8; json_len as usize];
    reader
        .read_exact(&mut json_buf)
        .map_err(|e| Error::Input(format!("failed to read json body: {e}")))?;

    let body: PublishBody = crate::json::from_slice(&json_buf)
        .map_err(|e| Error::Input(format!("invalid publish json: {e}")))?;

    let mut crate_len_buf = [0u8; 4];
    reader
        .read_exact(&mut crate_len_buf)
        .map_err(|e| Error::Input(format!("failed to read crate length: {e}")))?;
    let crate_len = u32::from_le_bytes(crate_len_buf) as u64;

    if crate_len == 0 {
        return Err(Error::Input("empty crate archive".to_string()));
    }
    if crate_len > max_compressed {
        return Err(Error::Limit("crate archive exceeds max compressed size".to_string()));
    }
    if 4 + json_len + 4 + crate_len != content_length {
        return Err(Error::Input("crate length does not match content length framing".to_string()));
    }

    let spill = spill_path(staging_dir);
    let mut staged_file = OpenOptions::new().create_new(true).write(true).open(&spill)?;
    let cleanup = TmpCleanup(spill.clone());
    let mut hasher = Sha256::new();
    let mut remaining = crate_len;
    let mut buf = vec![0u8; 65536];
    while remaining > 0 {
        let want = (remaining.min(buf.len() as u64)) as usize;
        if let Err(e) = reader
            .read_exact(&mut buf[..want])
            .map_err(|e| Error::Input(format!("failed to read crate bytes: {e}")))
        {
            drop(staged_file);
            drop(cleanup);
            return Err(e);
        }
        hasher.update(&buf[..want]);
        if let Err(e) = staged_file
            .write_all(&buf[..want])
            .map_err(|e| Error::Operation(format!("failed to stage upload: {e}")))
        {
            drop(staged_file);
            drop(cleanup);
            return Err(e);
        }
        remaining -= want as u64;
    }
    if let Err(e) = staged_file.sync_all().map_err(Error::from) {
        drop(staged_file);
        drop(cleanup);
        return Err(e);
    }
    drop(staged_file);
    let crate_sha256 = format!("{:x}", hasher.finalize_reset());

    Ok(StagedUpload {
        body,
        crate_bytes_path: spill.clone(),
        crate_len,
        crate_sha256,
        _cleanup: cleanup,
    })
}
