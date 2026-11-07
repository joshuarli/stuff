#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use stuff::publish::config::Config;
use stuff::publish::paths::Paths;
use stuff::publish::r2::credentials::resolve;
use stuff::publish::r2::parallel::{REQUEST_CONCURRENCY, map_bounded};
use stuff::publish::r2::put::{PutAction, UploadOptions};
use stuff::publish::r2::sync::{Payload, PlanEntry, SitePlan, execute_with_concurrency};
use stuff::publish::r2::{R2Client, RawRequest};
use stuff::publish::{Environment, PublishError};

const SCRATCH_ROOT: &str = "bench/stuf-62/";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("r2_upload_bench: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut concurrency = REQUEST_CONCURRENCY;
    let mut sizes_from: Option<PathBuf> = None;
    let mut scratch: Option<PathBuf> = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--concurrency" => concurrency = value.parse().map_err(|e| format!("{flag}: {e}"))?,
            "--sizes-from" => sizes_from = Some(PathBuf::from(value)),
            "--scratch" => scratch = Some(PathBuf::from(value)),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    let sizes_from = sizes_from.ok_or("--sizes-from DIR is required")?;
    let scratch = scratch.ok_or("--scratch DIR is required")?;

    let environment = Environment::from_pairs(std::env::vars());
    let paths = Paths::from_environment(&environment);
    let config = Config::load(&paths.config_file()).map_err(|e| e.to_string())?;
    let bucket = config.cargo.bucket.clone();
    let credentials =
        resolve(&environment, &config.cf_profile, &bucket).map_err(|e| e.to_string())?;
    let client = R2Client::new(credentials);

    let mut sizes = Vec::new();
    collect_sizes(&sizes_from, &mut sizes)?;
    sizes.sort_unstable();

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let prefix = format!("{SCRATCH_ROOT}{nonce:x}-{:x}/", std::process::id());
    let site = scratch.join(format!("site-{nonce:x}"));
    let mut entries = Vec::new();
    let mut state = nonce as u64 | 1;
    for (index, size) in sizes.iter().enumerate() {
        let key = format!("{prefix}f{index:05}.json");
        let path = site.join(&key);
        std::fs::create_dir_all(path.parent().expect("bench key has a parent"))
            .map_err(|e| e.to_string())?;
        let mut body = vec![0u8; *size as usize];
        for chunk in body.chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let bytes = state.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        std::fs::write(&path, &body).map_err(|e| e.to_string())?;
        entries.push(PlanEntry {
            key: key.clone(),
            size: *size,
            action: PutAction::Put,
            payload: Payload::File(PathBuf::from(key)),
        });
    }
    let plan = SitePlan {
        generation: String::new(),
        entries,
        aliases: Vec::new(),
    };

    let started = Instant::now();
    let uploaded = execute_with_concurrency(
        &client,
        &site,
        &bucket,
        &plan,
        &UploadOptions::default(),
        concurrency,
    );
    let upload_seconds = started.elapsed().as_secs_f64();

    let keys: Vec<String> = plan.entries.iter().map(|entry| entry.key.clone()).collect();
    let cleaned = delete_all(&client, &bucket, &keys);
    let leftover = client
        .list_objects_v2(&bucket)
        .map(|objects| {
            objects
                .iter()
                .filter(|object| object.key.starts_with(&prefix))
                .count()
        })
        .map_err(|e| e.to_string());
    std::fs::remove_dir_all(&site).map_err(|e| e.to_string())?;

    let report = uploaded.map_err(|e| e.to_string())?;
    cleaned.map_err(|e| e.to_string())?;
    let leftover = leftover?;
    println!(
        "concurrency={concurrency} objects={} bytes={} upload_seconds={upload_seconds:.3} leftover={leftover}",
        report.uploaded_objects(),
        report.uploaded_bytes
    );
    if leftover != 0 {
        return Err(format!("{leftover} objects remain under {prefix}"));
    }
    Ok(())
}

fn delete_all(client: &R2Client, bucket: &str, keys: &[String]) -> Result<(), PublishError> {
    map_bounded(keys, 16, |key| {
        if !key.starts_with(SCRATCH_ROOT) {
            return Err(PublishError::Usage(format!("refusing to delete {key}")));
        }
        let response = client.request(RawRequest {
            method: "DELETE",
            bucket,
            key,
            query: Vec::new(),
            body: &[],
            content_type: None,
            extra_headers: Vec::new(),
        })?;
        if response.status / 100 != 2 {
            return Err(PublishError::Transport(format!(
                "DELETE {key}: HTTP {}",
                response.status
            )));
        }
        Ok(())
    })
    .map(|_| ())
}

fn collect_sizes(directory: &Path, sizes: &mut Vec<u64>) -> Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            collect_sizes(&entry.path(), sizes)?;
        } else if kind.is_file() {
            sizes.push(entry.metadata().map_err(|e| e.to_string())?.len());
        }
    }
    Ok(())
}
