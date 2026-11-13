use std::io::Write as _;
use std::path::{Path, PathBuf};



use crate::acquire::{self, Acquire};
use crate::archive::extract;
use crate::error::{Error, Result};
use crate::acquire::sha256_hex;
use crate::json::{canonical_string, render_pretty, seq, string_map};
use crate::manifest::Manifest;
use crate::zip::write_zip;

pub const RENDERER_VERSION: u64 = 1;
pub const SCHEMA_VERSION: u64 = 1;
pub const REGISTRY: &str = "mirror";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltArtifact {
    pub id: String,
    pub version: String,
    pub upstream_url: String,
    pub upstream_sha256: String,
    pub strip_components: u64,
    pub keep: Option<Vec<String>>,
    pub file: String,
    pub files: Vec<String>,
    pub sha256: String,
    pub bytes: u64,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltMirror {
    pub generation: String,
    pub artifacts: Vec<BuiltArtifact>,
    pub catalog: String,
    pub sums: String,
}

pub fn generation_id(manifest: &Manifest, base_url: &str) -> Result<String> {
    let artifacts = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            let mut entries = vec![
                ("id", deser_value::Value::from(artifact.id.clone())),
                ("sha256", deser_value::Value::from(artifact.sha256.clone())),
                ("stripComponents", deser_value::Value::from(artifact.strip_components)),
                ("url", deser_value::Value::from(artifact.url.clone())),
                ("version", deser_value::Value::from(artifact.version.clone())),
            ];
            if let Some(keep) = &artifact.keep {
                let mut keep = keep.clone();
                keep.sort();
                entries.push((
                    "keep",
                    seq(keep.into_iter().map(deser_value::Value::from).collect()),
                ));
            }
            string_map(entries)
        })
        .collect();
    let payload = string_map(vec![
        ("artifacts", seq(artifacts)),
        ("baseUrl", deser_value::Value::from(normalize_base_url(base_url))),
        ("renderer", deser_value::Value::from(RENDERER_VERSION)),
        ("schemaVersion", deser_value::Value::from(SCHEMA_VERSION)),
    ]);
    let canonical = canonical_string(&payload)?;
    Ok(sha256_hex(canonical.as_bytes()))
}

pub fn normalize_base_url(base_url: &str) -> String {
    if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    }
}

pub fn build(
    manifest: &Manifest,
    base_url: &str,
    acquire: &dyn Acquire,
    cache_dir: Option<&Path>,
    staging_dir: &Path,
) -> Result<BuiltMirror> {
    std::fs::create_dir_all(staging_dir)?;
    let mut artifacts = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        let bytes = acquire::fetch_verified(acquire, cache_dir, &artifact.url, &artifact.sha256)?;
        let members = extract(artifact, &bytes)?;
        let zip = write_zip(&members)?;
        let path = staging_dir.join(format!("{}.zip", artifact.id));
        let mut file = std::fs::File::create(&path)?;
        file.write_all(&zip)?;
        let sha256 = sha256_hex(&zip);
        artifacts.push(BuiltArtifact {
            id: artifact.id.clone(),
            version: artifact.version.clone(),
            upstream_url: artifact.url.clone(),
            upstream_sha256: artifact.sha256.clone(),
            strip_components: artifact.strip_components,
            keep: artifact.keep.clone(),
            file: format!("{}.zip", artifact.id),
            files: members.into_iter().map(|member| member.path).collect(),
            sha256,
            bytes: zip.len() as u64,
            path,
        });
    }
    let generation = generation_id(manifest, base_url)?;
    let base_url = normalize_base_url(base_url);
    let catalog = render_catalog(&generation, &base_url, &artifacts)?;
    let sums = render_sums(&artifacts);
    Ok(BuiltMirror {
        generation,
        artifacts,
        catalog,
        sums,
    })
}

fn render_catalog(generation: &str, base_url: &str, artifacts: &[BuiltArtifact]) -> Result<String> {
    let artifact_values = artifacts
        .iter()
        .map(|artifact| {
            let mut entries = vec![
                ("bytes", deser_value::Value::from(artifact.bytes)),
                ("file", deser_value::Value::from(artifact.file.clone())),
                (
                    "files",
                    seq(artifact
                        .files
                        .iter()
                        .cloned()
                        .map(deser_value::Value::from)
                        .collect()),
                ),
                ("id", deser_value::Value::from(artifact.id.clone())),
                ("sha256", deser_value::Value::from(artifact.sha256.clone())),
                (
                    "stripComponents",
                    deser_value::Value::from(artifact.strip_components),
                ),
                (
                    "upstream",
                    string_map(vec![
                        ("sha256", deser_value::Value::from(artifact.upstream_sha256.clone())),
                        ("url", deser_value::Value::from(artifact.upstream_url.clone())),
                    ]),
                ),
                ("version", deser_value::Value::from(artifact.version.clone())),
            ];
            if let Some(keep) = &artifact.keep {
                entries.push((
                    "keep",
                    seq(keep
                        .iter()
                        .cloned()
                        .map(deser_value::Value::from)
                        .collect()),
                ));
            }
            string_map(entries)
        })
        .collect();
    let packages = artifacts
        .iter()
        .map(|artifact| {
            string_map(vec![
                ("name", deser_value::Value::from(artifact.id.clone())),
                ("source", deser_value::Value::from("upstream".to_string())),
                (
                    "versions",
                    seq(vec![string_map(vec![(
                        "version",
                        deser_value::Value::from(artifact.version.clone()),
                    )])]),
                ),
            ])
        })
        .collect();
    let catalog = string_map(vec![
        ("artifacts", seq(artifact_values)),
        ("baseUrl", deser_value::Value::from(base_url.to_string())),
        ("generation", deser_value::Value::from(generation.to_string())),
        ("packages", seq(packages)),
        ("registry", deser_value::Value::from(REGISTRY.to_string())),
        ("renderer", deser_value::Value::from(RENDERER_VERSION)),
        ("schemaVersion", deser_value::Value::from(SCHEMA_VERSION)),
    ]);
    render_pretty(&catalog)
}

fn render_sums(artifacts: &[BuiltArtifact]) -> String {
    let mut lines: Vec<&BuiltArtifact> = artifacts.iter().collect();
    lines.sort_by(|left, right| left.file.cmp(&right.file));
    let mut out = String::new();
    for artifact in lines {
        out.push_str(&format!("{}  {}\n", artifact.sha256, artifact.file));
    }
    out
}

/// Stages the generation inside `site_dir` and switches its `current` link,
/// reusing the shared store layout. Returns true when the generation was
/// already installed and verified.
pub fn assemble(site_dir: &Path, built: &BuiltMirror) -> Result<bool> {
    let site = stuff_store::Site::new(site_dir.to_path_buf());
    site.check_no_symlinks_in_path()
        .map_err(|error| Error::operation(format!("{}: {error}", site_dir.display())))?;
    site.recover_staging()
        .map_err(|error| Error::operation(format!("{}: {error}", site_dir.display())))?;
    site.ensure_layout()
        .map_err(|error| Error::operation(format!("{}: {error}", site_dir.display())))?;
    let expected = built.artifacts.clone();
    let catalog = built.catalog.clone();
    let sums = built.sums.clone();
    let verify_catalog = catalog.clone();
    let verify_sums = sums.clone();
    let publish = site
        .publish(
            &built.generation,
            &|staged| {
                for artifact in &expected {
                    move_or_copy(&artifact.path, &staged.join(&artifact.file))?;
                }
                std::fs::write(staged.join("catalog.json"), catalog.as_bytes())?;
                std::fs::write(staged.join("SHA256SUMS"), sums.as_bytes())?;
                Ok(())
            },
            &|directory| Ok(verify_generation(directory, &expected, &verify_catalog, &verify_sums)),
        )
        .map_err(|error| match error {
            stuff_store::Error::Integrity(detail) => Error::integrity(detail),
            other => Error::operation(format!("{other}")),
        })?;
    Ok(publish.is_noop)
}

fn verify_generation(
    directory: &Path,
    artifacts: &[BuiltArtifact],
    catalog: &str,
    sums: &str,
) -> bool {
    let read = |name: &str| std::fs::read_to_string(directory.join(name)).ok();
    let Some(actual_sums) = read("SHA256SUMS") else {
        return false;
    };
    let Some(actual_catalog) = read("catalog.json") else {
        return false;
    };
    if actual_catalog != catalog || actual_sums != sums {
        return false;
    }
    artifacts.iter().all(|artifact| {
        std::fs::read(directory.join(&artifact.file))
            .map(|bytes| {
                bytes.len() as u64 == artifact.bytes
                    && sha256_hex(&bytes) == artifact.sha256
            })
            .unwrap_or(false)
    })
}

fn move_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ())
}
