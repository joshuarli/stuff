#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use h12tiny_client_sync::{http, Client};
use stuff_oci::{parse_image, Digest, Image, RepositoryName, Tag};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const CONCURRENCIES: [usize; 3] = [1, 8, 32];
const MANIFEST_REQUESTS_PER_THREAD: usize = 16;
const BLOB_SIZES: [u64; 4] = [
    1024 * 1024,
    64 * 1024 * 1024,
    256 * 1024 * 1024,
    1024 * 1024 * 1024,
];
const MIB: f64 = 1024.0 * 1024.0;

#[derive(Debug, Clone)]
pub struct BenchOptions {
    pub base_url: String,
    pub repository: RepositoryName,
    pub tag: Tag,
    pub output: PathBuf,
    pub runs: usize,
    pub label: String,
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    received: u64,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn client() -> Client {
    Client::builder().connect_timeout(CONNECT_TIMEOUT).build()
}

fn request(
    client: &Client,
    url: &str,
    method: http::Method,
    accept: Option<&str>,
    capture: bool,
) -> Result<HttpResponse> {
    let mut current = url.to_string();
    for _ in 0..6 {
        let response = request_once(client, &current, method.clone(), accept, capture)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = response.header("location")
        {
            let base = stuff_url::Url::parse(&current)
                .map_err(|error| Error::Network(format!("invalid URL {current}: {error}")))?;
            current = base
                .join(location)
                .map_err(|error| Error::Network(format!("invalid redirect {location}: {error}")))?
                .to_string();
            continue;
        }
        return Ok(response);
    }
    Err(Error::Network(format!("too many redirects for {url}")))
}

fn request_once(
    client: &Client,
    url: &str,
    method: http::Method,
    accept: Option<&str>,
    capture: bool,
) -> Result<HttpResponse> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(url.to_string())
        .header("user-agent", "myoci-bench/0.1.0");
    if let Some(accept) = accept {
        builder = builder.header("accept", accept);
    }
    let req = builder
        .body(Vec::new())
        .map_err(|e| Error::Network(format!("request for {url}: {e}")))?;
    let mut response = client
        .request_with_timeout(req, Some(REQUEST_TIMEOUT))
        .map_err(|e| Error::Network(format!("fetch {url}: {e}")))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let mut body = Vec::new();
    let mut received = 0u64;
    {
        let reader = response.body_mut();
        let _ = reader.set_read_timeout(Some(REQUEST_TIMEOUT));
        let mut buffer = [0u8; 1 << 20];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|e| Error::Network(format!("read {url}: {e}")))?;
            if read == 0 {
                break;
            }
            received += read as u64;
            if capture {
                body.extend_from_slice(&buffer[..read]);
            }
        }
    }
    Ok(HttpResponse { status, headers, body, received })
}

fn accept() -> &'static str {
    "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json"
}

fn manifest_url(base: &str, repository: &RepositoryName, reference: &str) -> String {
    format!("{base}/v2/{}/manifests/{reference}", repository.as_str())
}

fn blob_url(base: &str, repository: &RepositoryName, digest: &Digest) -> String {
    format!("{base}/v2/{}/blobs/{}", repository.as_str(), digest.as_str())
}

fn check_base(base: &str) -> Result<String> {
    let base = base.trim_end_matches('/').to_string();
    let parsed: stuff_url::Url = base
        .parse()
        .map_err(|_| Error::Input(format!("invalid base URL: {base}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::Input("base URL must not carry credentials".to_string()));
    }
    let loopback = matches!(
        parsed.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]")
    );
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(Error::Input(
            "base URL must be HTTPS or loopback HTTP".to_string(),
        ));
    }
    Ok(base)
}

#[derive(Default)]
struct Samples {
    micros: Vec<u128>,
}

impl Samples {
    fn push(&mut self, elapsed: Duration) {
        self.micros.push(elapsed.as_micros());
    }

    fn summary(&self) -> Summary {
        let mut values = self.micros.clone();
        values.sort_unstable();
        Summary {
            p50: percentile(&values, 50.0),
            p95: percentile(&values, 95.0),
            p99: percentile(&values, 99.0),
            count: values.len(),
        }
    }
}

struct Summary {
    p50: f64,
    p95: f64,
    p99: f64,
    count: usize,
}

fn percentile(sorted: &[u128], percent: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((percent / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)] as f64 / 1000.0
}

struct BlobResult {
    elapsed: Duration,
    bytes: u64,
}

fn download_blob(client: &Client, url: &str) -> Result<BlobResult> {
    let started = Instant::now();
    let response = request(client, url, http::Method::GET, None, false)?;
    if response.status != 200 {
        return Err(Error::Network(format!("{url}: HTTP {}", response.status)));
    }
    Ok(BlobResult { elapsed: started.elapsed(), bytes: response.received })
}

fn measure_manifest(
    base: &str,
    repository: &RepositoryName,
    reference: &str,
    method: http::Method,
    concurrency: usize,
) -> Result<Samples> {
    let url = manifest_url(base, repository, reference);
    let mut samples = Samples::default();
    let per_thread = MANIFEST_REQUESTS_PER_THREAD;
    let results: Vec<Result<Vec<u128>>> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..concurrency {
            let url = url.clone();
            let method = method.clone();
            handles.push(scope.spawn(move || {
                let client = client();
                let mut local = Vec::with_capacity(per_thread);
                for _ in 0..per_thread {
                    let started = Instant::now();
                    let response = request(&client, &url, method.clone(), Some(accept()), false)?;
                    if response.status != 200 {
                        return Err(Error::Network(format!("{url}: HTTP {}", response.status)));
                    }
                    local.push(started.elapsed().as_micros());
                }
                Ok(local)
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| Error::Operation("benchmark thread panicked".to_string()))?
            })
            .collect()
    });
    for result in results {
        for micros in result? {
            samples.micros.push(micros);
        }
    }
    Ok(samples)
}

struct Graph {
    manifests: Vec<(String, Vec<u8>)>,
    blobs: BTreeSet<Digest>,
}

fn graph(
    base: &str,
    repository: &RepositoryName,
    tag: &Tag,
) -> Result<Graph> {
    let client = client();
    let response = request(
        &client,
        &manifest_url(base, repository, tag.as_str()),
        http::Method::GET,
        Some(accept()),
        true,
    )?;
    if response.status != 200 {
        return Err(Error::Network(format!(
            "manifest {}: HTTP {}",
            tag.as_str(),
            response.status
        )));
    }
    let root = parse_image(&response.body)
        .map_err(|error| Error::Integrity(format!("benchmark manifest: {error}")))?;
    let mut manifests = vec![(format!("manifests/{}", tag.as_str()), response.body)];
    let mut blobs = BTreeSet::new();
    match &root {
        Image::Index(index) => {
            for child in &index.manifests {
                let child_response = request(
                    &client,
                    &manifest_url(base, repository, child.digest.as_str()),
                    http::Method::GET,
                    Some(accept()),
                    true,
                )?;
                if child_response.status != 200 {
                    return Err(Error::Network(format!(
                        "child manifest {}: HTTP {}",
                        child.digest,
                        child_response.status
                    )));
                }
                let image = parse_image(&child_response.body)
                    .map_err(|error| Error::Integrity(format!("child manifest: {error}")))?;
                collect_blobs(&image, &mut blobs);
                manifests.push((format!("manifests/{}", child.digest.as_str()), child_response.body));
            }
        }
        Image::Manifest(_) => {
            collect_blobs(&root, &mut blobs);
        }
    }
    Ok(Graph { manifests, blobs })
}

fn collect_blobs(image: &Image, blobs: &mut BTreeSet<Digest>) {
    if let Image::Manifest(manifest) = image {
        blobs.insert(manifest.config.digest.clone());
        for layer in &manifest.layers {
            blobs.insert(layer.digest.clone());
        }
    }
}

fn blob_sizes(
    base: &str,
    repository: &RepositoryName,
    tag: &Tag,
) -> Result<std::collections::BTreeMap<u64, Digest>> {
    let client = client();
    let response = request(
        &client,
        &manifest_url(base, repository, tag.as_str()),
        http::Method::GET,
        Some(accept()),
        true,
    )?;
    let root = parse_image(&response.body)
        .map_err(|error| Error::Integrity(format!("benchmark manifest: {error}")))?;
    let mut sizes = std::collections::BTreeMap::new();
    let mut add = |image: &Image| {
        if let Image::Manifest(manifest) = image {
            for layer in &manifest.layers {
                sizes.entry(layer.size).or_insert_with(|| layer.digest.clone());
            }
        }
    };
    match &root {
        Image::Index(index) => {
            for child in &index.manifests {
                let response = request(
                    &client,
                    &manifest_url(base, repository, child.digest.as_str()),
                    http::Method::GET,
                    Some(accept()),
                    true,
                )?;
                if response.status == 200
                    && let Ok(image) = parse_image(&response.body)
                {
                    add(&image);
                }
            }
        }
        Image::Manifest(_) => add(&root),
    }
    Ok(sizes)
}

fn environment() -> String {
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "rustc unknown".to_string());
    let uname = std::process::Command::new("uname")
        .args(["-srm"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH));
    format!("{uname}; {rustc}")
}

fn timestamp() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            format!("unix:{seconds}")
        })
}

pub fn run(options: &BenchOptions) -> Result<()> {
    if options.runs == 0 || options.runs > 10 {
        return Err(Error::Input("--runs must be between 1 and 10".to_string()));
    }
    let base = check_base(&options.base_url)?;
    let mut out = String::new();
    out.push_str("# OCI benchmarks\n\n");
    out.push_str(&format!("- Command: `myoci bench --base-url {} --repo {} --tag {} --runs {} --label {}`\n", base, options.repository.as_str(), options.tag.as_str(), options.runs, options.label));
    out.push_str(&format!("- Label: {}\n", options.label));
    out.push_str(&format!("- Host: {}\n", environment()));
    out.push_str(&format!("- Date: {}\n", timestamp()));
    out.push_str(&format!("- Runs: {}\n", options.runs));
    out.push_str("- Method: local loopback unless the base URL is deployed. Cold is the first pass with a new client; warm repeats with the same client. On the deployed Worker, cold means a freshly published blob digest because the gateway cache key ignores query strings. The 1 GiB row is a local measurement; the Worker's 512 MB cacheable-object cap applies only to the deployed path (Needs environment).\n\n");

    // Manifest GET/HEAD matrix.
    let mut manifest_rows = Vec::new();
    for concurrency in CONCURRENCIES {
        for (method_name, method) in [("GET", http::Method::GET), ("HEAD", http::Method::HEAD)] {
            let mut cold = Samples::default();
            let mut warm = Samples::default();
            for run in 0..options.runs {
                let samples = measure_manifest(
                    &base,
                    &options.repository,
                    options.tag.as_str(),
                    method.clone(),
                    concurrency,
                )?;
                if run == 0 {
                    cold = samples;
                } else {
                    warm.micros.extend(samples.micros);
                }
            }
            let cold_summary = cold.summary();
            let warm_summary = warm.summary();
            manifest_rows.push((method_name, concurrency, "cold (run 1)", cold_summary));
            manifest_rows.push((method_name, concurrency, "warm", warm_summary));
        }
    }

    // Resolve the image graph once.
    let sizes = blob_sizes(&base, &options.repository, &options.tag)?;
    let mut blob_rows = Vec::new();
    for size in BLOB_SIZES {
        let Some(digest) = sizes.get(&size) else {
            return Err(Error::Input(format!(
                "benchmark image has no {size}-byte layer; run deploy/oci-bench.sh to build the size matrix"
            )));
        };
        let url = blob_url(&base, &options.repository, digest);
        let mut cold = Samples::default();
        let mut warm = Samples::default();
        for _ in 0..options.runs {
            let cold_client = client();
            let cold_result = download_blob(&cold_client, &url)?;
            if cold_result.bytes != size {
                return Err(Error::Integrity(format!(
                    "blob {digest} returned {} bytes, expected {size}",
                    cold_result.bytes
                )));
            }
            cold.push(cold_result.elapsed);
            let warm_result = download_blob(&cold_client, &url)?;
            warm.push(warm_result.elapsed);
        }
        blob_rows.push((size, "cold", cold, size));
        blob_rows.push((size, "warm", warm, size));
    }

    // Full image pull: manifests plus every unique blob, twice per run.
    let mut pull_rows: Vec<(&str, Samples, u64)> = Vec::new();
    let mut first = Samples::default();
    let mut repeat = Samples::default();
    let mut total_bytes = 0u64;
    for _ in 0..options.runs {
        for pass in 0..2 {
            let started = Instant::now();
            let client = client();
            let mut bytes = 0u64;
            let pull = graph(&base, &options.repository, &options.tag)?;
            bytes += pull.manifests.iter().map(|(_, body)| body.len() as u64).sum::<u64>();
            for blob in &pull.blobs {
                let result = download_blob(&client, &blob_url(&base, &options.repository, blob))?;
                bytes += result.bytes;
            }
            let elapsed = started.elapsed();
            total_bytes = bytes;
            if pass == 0 {
                first.push(elapsed);
            } else {
                repeat.push(elapsed);
            }
        }
    }
    pull_rows.push(("first pull", first, total_bytes));
    pull_rows.push(("repeat pull", repeat, total_bytes));

    out.push_str("## Manifest requests\n\n");
    out.push_str("| Method | Concurrency | Temperature | Requests | p50 ms | p95 ms | p99 ms |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
    for (method, concurrency, temperature, summary) in &manifest_rows {
        out.push_str(&format!(
            "| {method} | {concurrency} | {temperature} | {} | {:.3} | {:.3} | {:.3} |\n",
            summary.count, summary.p50, summary.p95, summary.p99
        ));
    }
    out.push_str("\n## Blob downloads\n\n");
    out.push_str("| Size | Temperature | Samples | p50 ms | p95 ms | p99 ms | MiB/s |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
    for (size, temperature, samples, bytes) in &blob_rows {
        let summary = samples.summary();
        let total_seconds: f64 = samples.micros.iter().map(|micros| *micros as f64 / 1_000_000.0).sum();
        let throughput = if total_seconds > 0.0 {
            (*bytes as f64 * summary.count as f64) / total_seconds / MIB
        } else {
            0.0
        };
        out.push_str(&format!(
            "| {} MiB | {temperature} | {} | {:.3} | {:.3} | {:.3} | {throughput:.1} |\n",
            size / (1024 * 1024),
            summary.count,
            summary.p50,
            summary.p95,
            summary.p99
        ));
    }
    out.push_str("\n## Full image pull\n\n");
    out.push_str("| Pass | Samples | p50 ms | p95 ms | p99 ms | MiB/s |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for (label, samples, bytes) in &pull_rows {
        let summary = samples.summary();
        let total_seconds: f64 = samples.micros.iter().map(|micros| *micros as f64 / 1_000_000.0).sum();
        let throughput = if total_seconds > 0.0 {
            (*bytes as f64 * summary.count as f64) / total_seconds / MIB
        } else {
            0.0
        };
        out.push_str(&format!(
            "| {label} | {} | {:.3} | {:.3} | {:.3} | {throughput:.1} |\n",
            summary.count, summary.p50, summary.p95, summary.p99
        ));
    }
    out.push('\n');
    if let Some(parent) = options.output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    write_output(&options.output, &out)?;
    println!("wrote {}", options.output.display());
    Ok(())
}

fn write_output(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text)
        .map_err(|error| Error::Operation(format!("could not write {}: {error}", path.display())))
}
