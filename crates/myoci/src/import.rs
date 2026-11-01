#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::State;
use stuff_oci::{
    parse_image, Descriptor, Digest, Image, RepositoryName, Tag,
};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
pub const REF_NAME_ANNOTATION: &str = "org.opencontainers.image.ref.name";
pub const ORAS_VERSION: &str = "1.3.6";
pub const ALPINE_SAMPLE: &str =
    "docker.io/library/alpine@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    pub root: Digest,
    pub tag: Tag,
    pub manifests: usize,
    pub blobs: usize,
    pub referrers: usize,
}

pub fn import_layout(
    state: &State,
    layout: &std::path::Path,
    repository: &RepositoryName,
    tag: Option<&Tag>,
) -> Result<ImportSummary> {
    state.init()?;
    let roots = read_layout_index(layout)?;
    let (root, root_tag) = select_root(&roots, tag)?;
    let mut walk = Walk::default();
    for descriptor in &roots {
        walk_descriptor(layout, descriptor, &mut walk)?;
    }
    if !walk.manifests.contains_key(&root.digest) {
        return Err(Error::Integrity(format!(
            "layout index root {} was not imported",
            root.digest
        )));
    }

    let mut view = state
        .read_repository(repository)?
        .unwrap_or_else(|| stuff_oci::RepositoryView::new(repository.clone()));
    for (digest, (descriptor, _)) in &walk.manifests {
        if descriptor.digest != *digest {
            return Err(Error::Integrity("manifest descriptor digest mismatch".to_string()));
        }
        view.insert_manifest(descriptor.clone());
    }
    if let Some(existing) = view.tags.get(&root_tag)
        && existing.digest != root.digest
    {
        return Err(Error::Conflict(format!(
            "tag {} already points at {}; refusing to replace it with {}",
            root_tag.as_str(),
            existing.digest,
            root.digest
        )));
    }
    view.tag(root_tag.clone(), walk.manifests[&root.digest].0.clone());

    for digest in walk.blobs.iter() {
        let source = layout_blob_path(layout, digest);
        state.store_blob(&source, digest)?;
    }
    for (digest, (_, bytes)) in &walk.manifests {
        state.store_manifest(digest, bytes)?;
    }

    let referrers = state.rebuild_referrers(&view)?;

    let mut blobs = state.read_blobs()?;
    blobs.extend(walk.blobs.iter().cloned());
    state.write_blobs(&blobs)?;
    state.write_repository(&view)?;

    Ok(ImportSummary {
        root: root.digest,
        tag: root_tag,
        manifests: walk.manifests.len(),
        blobs: walk.blobs.len(),
        referrers,
    })
}

pub fn import_from(
    state: &State,
    reference: &str,
    repository: &RepositoryName,
    tag: Option<&Tag>,
) -> Result<ImportSummary> {
    state.init()?;
    let oras = oras_binary()?;
    let version = Command::new(&oras)
        .arg("version")
        .output()
        .map_err(|e| {
            Error::Operation(format!(
                "oras {ORAS_VERSION} is required: {e}; install with go install oras.land/oras/cmd/oras@{ORAS_VERSION}"
            ))
        })?;
    if !version.status.success() {
        return Err(Error::Operation(format!(
            "oras {ORAS_VERSION} is required; `oras version` failed"
        )));
    }
    let stdout = String::from_utf8_lossy(&version.stdout);
    let found = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Version:"))
        .map(str::trim)
        .unwrap_or("unknown");
    if found != ORAS_VERSION {
        return Err(Error::Operation(format!(
            "oras {ORAS_VERSION} is required; found {found}; install with go install oras.land/oras/cmd/oras@{ORAS_VERSION}"
        )));
    }

    let scratch = Scratch::new(state.root().join(format!(
        ".import.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    )))?;
    let layout_tag = tag
        .map(|tag| tag.as_str().to_string())
        .or_else(|| reference_tag(reference).map(str::to_string))
        .unwrap_or_else(|| "latest".to_string());
    let destination = format!("{}:{layout_tag}", scratch.path().display());
    let output = Command::new(&oras)
        .args(["cp", "--to-oci-layout", reference, &destination])
        .output()
        .map_err(|e| Error::Operation(format!("could not run oras: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Network(format!(
            "oras cp {reference} failed: {}",
            stderr.trim()
        )));
    }
    import_layout(state, scratch.path(), repository, tag)
}

fn oras_binary() -> Result<String> {
    if let Ok(value) = std::env::var("ORAS")
        && !value.is_empty()
    {
        return Ok(value);
    }
    Ok("oras".to_string())
}

fn reference_tag(reference: &str) -> Option<&str> {
    let without_digest = reference.split('@').next().unwrap_or(reference);
    let last = without_digest.rsplit('/').next()?;
    last.rsplit_once(':').map(|(_, tag)| tag)
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(path: std::path::PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&path)
            .map_err(|e| Error::Operation(format!("could not create {}: {e}", path.display())))?;
        Ok(Self(path))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Walk {
    manifests: BTreeMap<Digest, (Descriptor, Vec<u8>)>,
    blobs: BTreeSet<Digest>,
}

fn read_layout_index(layout: &std::path::Path) -> Result<Vec<Descriptor>> {
    let layout_text = std::fs::read_to_string(layout.join("oci-layout")).map_err(|_| {
        Error::Integrity(format!("{} is not an OCI image layout", layout.display()))
    })?;
    let layout_value: deser_value::Value = deser_json::from_str(&layout_text)
        .map_err(|e| Error::Integrity(format!("oci-layout is not valid JSON: {e}")))?;
    let version = layout_value
        .as_map()
        .and_then(|map| map.get("imageLayoutVersion"))
        .and_then(|value| value.as_str());
    if version != Some("1.0.0") {
        return Err(Error::Integrity(format!(
            "unsupported imageLayoutVersion: {}",
            version.unwrap_or("missing")
        )));
    }
    let index_text = std::fs::read_to_string(layout.join("index.json"))
        .map_err(|_| Error::Integrity(format!("{} has no index.json", layout.display())))?;
    let image = parse_image(index_text.as_bytes())
        .map_err(|error| Error::Integrity(format!("index.json: {error}")))?;
    match image {
        Image::Index(index) => Ok(index.manifests),
        Image::Manifest(_) => Err(Error::Integrity("index.json is not an OCI index".to_string())),
    }
}

fn select_root(roots: &[Descriptor], tag: Option<&Tag>) -> Result<(Descriptor, Tag)> {
    if roots.is_empty() {
        return Err(Error::Integrity("layout index.json has no manifests".to_string()));
    }
    let chosen = match tag {
        Some(tag) => roots
            .iter()
            .find(|descriptor| ref_name(descriptor) == Some(tag.as_str()))
            .or_else(|| roots.first()),
        None => roots
            .iter()
            .find(|descriptor| ref_name(descriptor).is_some())
            .or_else(|| roots.first()),
    }
    .expect("roots is not empty");
    let tag = match tag {
        Some(tag) => tag.clone(),
        None => match ref_name(chosen) {
            Some(name) => Tag::parse(name)
                .map_err(|error| Error::Integrity(format!("index.json ref name: {error}")))?,
            None => Tag::parse("latest").expect("latest is a valid tag"),
        },
    };
    Ok((chosen.clone(), tag))
}

fn ref_name(descriptor: &Descriptor) -> Option<&str> {
    descriptor.annotations.get(REF_NAME_ANNOTATION).map(String::as_str)
}

fn walk_descriptor(
    layout: &std::path::Path,
    descriptor: &Descriptor,
    walk: &mut Walk,
) -> Result<()> {
    if walk.manifests.contains_key(&descriptor.digest) {
        return Ok(());
    }
    let path = layout_blob_path(layout, &descriptor.digest);
    let bytes = std::fs::read(&path).map_err(|_| {
        Error::Integrity(format!(
            "manifest {} is missing from the layout",
            descriptor.digest
        ))
    })?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(Error::Limit(format!(
            "manifest {} exceeds the {MAX_MANIFEST_BYTES}-byte limit",
            descriptor.digest
        )));
    }
    if bytes.len() as u64 != descriptor.size {
        return Err(Error::Integrity(format!(
            "manifest {} size is {}, expected {}",
            descriptor.digest,
            bytes.len(),
            descriptor.size
        )));
    }
    let actual = Digest::from_bytes(&bytes);
    if actual != descriptor.digest {
        return Err(Error::Integrity(format!(
            "manifest digest is {actual}, expected {}",
            descriptor.digest
        )));
    }
    let image = parse_image(&bytes)
        .map_err(|error| Error::Integrity(format!("manifest {}: {error}", descriptor.digest)))?;
    let derived = image.descriptor(descriptor.digest.clone(), bytes.len() as u64);
    walk.manifests.insert(descriptor.digest.clone(), (derived, bytes));
    match &image {
        Image::Index(index) => {
            for child in &index.manifests {
                walk_descriptor(layout, child, walk)?;
            }
        }
        Image::Manifest(manifest) => {
            for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
                verify_blob(&layout_blob_path(layout, &blob.digest), blob)?;
                walk.blobs.insert(blob.digest.clone());
            }
        }
    }
    Ok(())
}

pub(crate) fn verify_blob(path: &std::path::Path, descriptor: &Descriptor) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|_| {
        Error::Integrity(format!("referenced blob {} is missing", descriptor.digest))
    })?;
    if metadata.len() != descriptor.size {
        return Err(Error::Integrity(format!(
            "blob {} size is {}, expected {}",
            descriptor.digest,
            metadata.len(),
            descriptor.size
        )));
    }
    let hash = stuff_store::stream_file_hash(path, stuff_store::HashAlg::Sha256)?;
    if hash != descriptor.digest.hex() {
        return Err(Error::Integrity(format!(
            "blob {} hashes to sha256:{hash}",
            descriptor.digest
        )));
    }
    Ok(())
}

fn layout_blob_path(layout: &std::path::Path, digest: &Digest) -> std::path::PathBuf {
    layout.join("blobs/sha256").join(digest.hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> RepositoryName {
        RepositoryName::parse("example/app").unwrap()
    }



    #[test]
    fn rejects_a_tag_that_would_change_bytes() {
        let state_dir = tempfile::tempdir().unwrap();
        let state = State::new(state_dir.path().to_path_buf());
        let layout = tempfile::tempdir().unwrap();
        let first = synthetic_layout(layout.path(), b"first");
        import_layout(&state, layout.path(), &repository(), Some(&Tag::parse("stable").unwrap()))
            .unwrap();

        let second = tempfile::tempdir().unwrap();
        synthetic_layout(second.path(), b"second");
        let error = import_layout(
            &state,
            second.path(),
            &repository(),
            Some(&Tag::parse("stable").unwrap()),
        )
        .unwrap_err();
        assert!(matches!(error, Error::Conflict(_)), "{error}");
        let view = state.read_repository(&repository()).unwrap().unwrap();
        assert_eq!(
            view.tags[&Tag::parse("stable").unwrap()].digest.as_str(),
            first
        );
    }

    #[test]
    fn rejects_missing_and_mismatched_blobs() {
        let state_dir = tempfile::tempdir().unwrap();
        let state = State::new(state_dir.path().to_path_buf());
        let layout = tempfile::tempdir().unwrap();
        let config = b"{}";
        let layer = b"layer";
        let manifest = manifest_with_config_and_layer(b"{}", b"layer");
        write_layout(layout.path(), &manifest, config, layer, true);
        std::fs::remove_file(layout.path().join("blobs/sha256").join(Digest::from_bytes(config).hex()))
            .unwrap();
        let error = import_layout(&state, layout.path(), &repository(), None).unwrap_err();
        assert!(matches!(error, Error::Integrity(_)), "{error}");

        let layout = tempfile::tempdir().unwrap();
        write_layout(layout.path(), &manifest, config, layer, true);
        let wrong = layout.path().join("blobs/sha256").join(Digest::from_bytes(layer).hex());
        std::fs::write(&wrong, b"other").unwrap();
        let error = import_layout(&state, layout.path(), &repository(), None).unwrap_err();
        assert!(matches!(error, Error::Integrity(_)), "{error}");
    }

    #[test]
    fn rejects_foreign_layers_and_schema_one() {
        let state_dir = tempfile::tempdir().unwrap();
        let state = State::new(state_dir.path().to_path_buf());
        let layout = tempfile::tempdir().unwrap();
        let foreign = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[{"mediaType":"application/vnd.docker.image.rootfs.foreign.diff.tar.gzip","digest":"sha256:6615b2d5776183ba25e1bfa8e789346fe3a65066e808ac5b6beb5710bb94d10e","size":53}]}"#;
        write_layout(layout.path(), foreign.as_bytes(), b"{}", b"", false);
        let error = import_layout(&state, layout.path(), &repository(), None).unwrap_err();
        assert!(matches!(error, Error::Integrity(_)), "{error}");

        let layout = tempfile::tempdir().unwrap();
        let schema1 = br#"{"schemaVersion":1,"mediaType":"application/vnd.docker.distribution.manifest.v1+json","fsLayers":[]}"#;
        write_layout(layout.path(), schema1, b"", b"", false);
        let error = import_layout(&state, layout.path(), &repository(), None).unwrap_err();
        assert!(matches!(error, Error::Integrity(_)), "{error}");
    }

    fn manifest_with_config_and_layer(config: &[u8], layer: &[u8]) -> Vec<u8> {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:{}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:{}","size":{}}}]}}"#,
            Digest::from_bytes(config).hex(),
            config.len(),
            Digest::from_bytes(layer).hex(),
            layer.len()
        )
        .into_bytes()
    }

    fn synthetic_layout(dir: &std::path::Path, layer: &[u8]) -> String {
        let manifest = manifest_with_config_and_layer(b"{}", layer);
        write_layout(dir, &manifest, b"{}", layer, true);
        Digest::from_bytes(&manifest).as_str().to_string()
    }

    fn write_layout(
        dir: &std::path::Path,
        manifest: &[u8],
        config: &[u8],
        layer: &[u8],
        tag: bool,
    ) {
        let blobs = dir.join("blobs/sha256");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(dir.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
        std::fs::write(blobs.join(Digest::from_bytes(manifest).hex()), manifest).unwrap();
        if !config.is_empty() {
            std::fs::write(blobs.join(Digest::from_bytes(config).hex()), config).unwrap();
        }
        if !layer.is_empty() {
            std::fs::write(blobs.join(Digest::from_bytes(layer).hex()), layer).unwrap();
        }
        let annotation = if tag {
            r#","annotations":{"org.opencontainers.image.ref.name":"stable"}"#.to_string()
        } else {
            String::new()
        };
        let index = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{}","size":{}{annotation}}}]}}"#,
            Digest::from_bytes(manifest).hex(),
            manifest.len()
        );
        std::fs::write(dir.join("index.json"), index).unwrap();
    }

}
