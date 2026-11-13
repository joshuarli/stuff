#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::State;
use crate::verify::verify_state_locked;
use stuff_catalog::{oci_catalog, render_catalog_json, render_html, render_llms_txt, OciInput};
use stuff_oci::{
    parse_blobs_json, parse_repository_json, render_blobs_json, render_repository_json, Digest,
    RepositoryName, RepositoryView,
};
use rustc_hash::{FxHashMap, FxHashSet};
use stuff_store::{HashAlg, Site};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_PUBLIC_URL: &str = "https://oci.17166969.xyz";
pub const LOCK_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishSummary {
    pub generation: String,
    pub no_op: bool,
    pub repositories: usize,
    pub manifests: usize,
    pub blobs: usize,
    pub referrers: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptSummary {
    pub generation: String,
    pub repositories: usize,
    pub manifests: usize,
    pub blobs: usize,
    pub referrers: usize,
}

pub fn publish(state: &State, site_dir: &Path, public_url: &str) -> Result<PublishSummary> {
    let _state_lock = state.lock()?;
    let _ = crate::mirror::transaction::recover_locked(state)?;
    publish_locked(state, site_dir, public_url)
}

pub(crate) fn publish_locked(state: &State, site_dir: &Path, public_url: &str) -> Result<PublishSummary> {
    let verified = verify_state_locked(state)?;
    let views = load_views(state)?;
    let (generation, files) = render_generation(&views, state, public_url)?;

    let manifest_digests: FxHashSet<Digest> = views
        .iter()
        .flat_map(|(_, view)| view.manifests.keys().cloned())
        .collect();
    let blobs = state.read_blobs()?;

    let site = Site::new(site_dir.to_path_buf());
    site.ensure_layout()?;
    let _lock = site.lock(LOCK_WAIT)?;
    site.recover_staging()?;
    site.check_no_symlinks_in_path()?;
    write_shared(state, site_dir, &manifest_digests, &blobs)?;

    let expected = files.clone();
    let build = |staging: &Path| -> stuff_store::Result<()> {
        for (relative, bytes) in &expected {
            stuff_store::write_file_durable(&staging.join(relative), bytes)?;
        }
        Ok(())
    };
    let verify = |dir: &Path| -> stuff_store::Result<bool> { tree_matches(dir, &expected) };
    let activation = site.publish(&generation, &build, &verify)?;
    verify_shared(state, site_dir, &manifest_digests, &blobs)?;

    Ok(PublishSummary {
        generation,
        no_op: activation.is_noop,
        repositories: views.len(),
        manifests: manifest_digests.len(),
        blobs: blobs.len(),
        referrers: verified.referrers,
    })
}

pub fn adopt(state: &State, site_dir: &Path) -> Result<AdoptSummary> {
    let _state_lock = state.lock()?;
    let _ = crate::mirror::transaction::recover_locked(state)?;
    state.init()?;
    let site = Site::new(site_dir.to_path_buf());
    let generation = site
        .current_generation()?
        .ok_or_else(|| Error::Input(format!("{} has no current generation", site_dir.display())))?;
    let generation_dir = site.generations_dir().join(&generation);
    let mut views = Vec::new();
    let mut manifest_digests = FxHashSet::default();
    for (name, path) in repository_files_under(&generation_dir.join("repositories"))? {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        let view = parse_repository_json(&name, &text)
            .map_err(|error| Error::Integrity(error.to_string()))?;
        manifest_digests.extend(view.manifests.keys().cloned());
        views.push((name, view));
    }
    let mut ordered_manifests: Vec<&Digest> = manifest_digests.iter().collect();
    ordered_manifests.sort_unstable();
    for digest in ordered_manifests {
        let path = site_dir.join("oci/manifests/sha256").join(digest.hex());
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        let actual = Digest::from_bytes(&bytes);
        if actual != *digest {
            return Err(Error::Integrity(format!("manifest {digest} hashes to {actual}")));
        }
        state.store_manifest(digest, &bytes)?;
    }
    let blobs_path = generation_dir.join("blobs.json");
    let blobs_text = std::fs::read_to_string(&blobs_path)
        .map_err(|e| Error::Operation(format!("could not read {}: {e}", blobs_path.display())))?;
    let blobs = parse_blobs_json(&blobs_text)
        .map_err(|error| Error::Integrity(error.to_string()))?;
    let mut ordered_blobs: Vec<&Digest> = blobs.iter().collect();
    ordered_blobs.sort_unstable();
    for digest in ordered_blobs {
        let source = site_dir.join("blobs/sha256").join(digest.hex());
        let actual = stuff_store::stream_file_hash(&source, HashAlg::Sha256)?;
        if actual != digest.hex() {
            return Err(Error::Integrity(format!("blob {digest} hashes to sha256:{actual}")));
        }
        state.store_blob(&source, digest)?;
    }
    state.write_blobs(&blobs)?;
    let mut referrers = 0;
    for (_, view) in &views {
        state.write_repository(view)?;
        referrers += state.rebuild_referrers(view)?;
    }
    if let Some(provenance) = crate::mirror::provenance::read_provenance(&generation_dir)? {
        let ownership = provenance.ownership_map()?;
        let rendered = crate::mirror::ownership::render_ownership(&ownership);
        stuff_store::write_file_durable(&state.root().join("mirror.json"), rendered.as_bytes())?;
        crate::mirror::provenance::write_provenance(state.root(), &provenance)?;
    }
    verify_state_locked(state)?;
    Ok(AdoptSummary {
        generation,
        repositories: views.len(),
        manifests: manifest_digests.len(),
        blobs: blobs.len(),
        referrers,
    })
}

fn load_views(state: &State) -> Result<Vec<(RepositoryName, RepositoryView)>> {
    let mut views = Vec::new();
    for (name, path) in state.repository_files()? {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        let view = parse_repository_json(&name, &text)
            .map_err(|error| Error::Integrity(error.to_string()))?;
        views.push((name, view));
    }
    Ok(views)
}

fn render_generation(
    views: &[(RepositoryName, RepositoryView)],
    state: &State,
    public_url: &str,
) -> Result<(String, FxHashMap<String, Vec<u8>>)> {
    let mut files = FxHashMap::default();
    let mut inputs = Vec::new();
    for (name, view) in views {
        let repository = format!("repositories/{}/repository.json", name.as_str());
        files.insert(repository, render_repository_json(view).into_bytes());
        let _ = state.rebuild_referrers(view)?;
        for subject in state.referrer_subjects(name)? {
            let path = state.referrers_path(name, &subject);
            let bytes = std::fs::read(&path)
                .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
            files.insert(
                format!(
                    "repositories/{}/referrers/sha256/{}.json",
                    name.as_str(),
                    subject.hex()
                ),
                bytes,
            );
        }
        inputs.push(OciInput {
            name: name.as_str().to_string(),
            tags: view.tags.keys().map(|tag| tag.as_str().to_string()).collect(),
        });
    }
    files.insert(
        "blobs.json".to_string(),
        render_blobs_json(&state.read_blobs()?).into_bytes(),
    );
    if let Some(provenance) = crate::mirror::provenance::read_provenance(state.root())? {
        files.insert(
            crate::mirror::provenance::PROVENANCE_FILE.to_string(),
            provenance.render().into_bytes(),
        );
    }
    let generation = stuff_oci::generation_id(&files);
    let catalog = oci_catalog(&generation, inputs)
        .map_err(|error| Error::Integrity(format!("catalog: {error}")))?;
    files.insert("catalog.json".to_string(), render_catalog_json(&catalog).into_bytes());
    files.insert("index.html".to_string(), render_html(&catalog).into_bytes());
    files.insert("llms.txt".to_string(), render_llms_txt(&catalog, public_url).into_bytes());
    Ok((generation, files))
}

fn write_shared(
    state: &State,
    site_dir: &Path,
    manifests: &FxHashSet<Digest>,
    blobs: &FxHashSet<Digest>,
) -> Result<()> {
    let manifest_dir = site_dir.join("oci/manifests/sha256");
    let blob_dir = site_dir.join("blobs/sha256");
    std::fs::create_dir_all(&manifest_dir)?;
    std::fs::create_dir_all(&blob_dir)?;
    let mut ordered_manifests: Vec<&Digest> = manifests.iter().collect();
    ordered_manifests.sort_unstable();
    for digest in ordered_manifests {
        let bytes = state.read_manifest(digest)?;
        if Digest::from_bytes(&bytes) != *digest {
            return Err(Error::Integrity(format!("manifest {digest} hashes to another digest")));
        }
        write_immutable(&manifest_dir.join(digest.hex()), &bytes)?;
    }
    let mut ordered_blobs: Vec<&Digest> = blobs.iter().collect();
    ordered_blobs.sort_unstable();
    for digest in ordered_blobs {
        let source = state.blob_path(digest);
        stuff_store::sync_immutable_file(
            &source,
            &blob_dir.join(digest.hex()),
            digest.hex(),
            HashAlg::Sha256,
        )?;
    }
    Ok(())
}

fn verify_shared(
    state: &State,
    site_dir: &Path,
    manifests: &FxHashSet<Digest>,
    blobs: &FxHashSet<Digest>,
) -> Result<()> {
    let mut ordered_manifests: Vec<&Digest> = manifests.iter().collect();
    ordered_manifests.sort_unstable();
    for digest in ordered_manifests {
        let path = site_dir.join("oci/manifests/sha256").join(digest.hex());
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        if Digest::from_bytes(&bytes) != *digest {
            return Err(Error::Integrity(format!("site manifest {digest} is corrupt")));
        }
        if state.read_manifest(digest)? != bytes {
            return Err(Error::Integrity(format!("site manifest {digest} differs from state")));
        }
    }
    let mut ordered_blobs: Vec<&Digest> = blobs.iter().collect();
    ordered_blobs.sort_unstable();
    for digest in ordered_blobs {
        let path = site_dir.join("blobs/sha256").join(digest.hex());
        let hash = stuff_store::stream_file_hash(&path, HashAlg::Sha256)?;
        if hash != digest.hex() {
            return Err(Error::Integrity(format!("site blob {digest} hashes to sha256:{hash}")));
        }
    }
    Ok(())
}

fn write_immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        let existing = std::fs::read(path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        if existing != bytes {
            return Err(Error::Integrity(format!(
                "{} already exists with different bytes",
                path.display()
            )));
        }
        return Ok(());
    }
    stuff_store::write_file_durable(path, bytes)?;
    Ok(())
}

fn tree_matches(root: &Path, expected: &FxHashMap<String, Vec<u8>>) -> stuff_store::Result<bool> {
    let mut actual = FxHashMap::default();
    collect_files(root, root, &mut actual)?;
    Ok(&actual == expected)
}

fn collect_files(
    dir: &Path,
    root: &Path,
    out: &mut FxHashMap<String, Vec<u8>>,
) -> stuff_store::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, root, out)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| stuff_store::Error::Integrity("tree walk escaped its root".to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(relative, std::fs::read(&path)?);
        }
    }
    Ok(())
}

fn repository_files_under(root: &Path) -> Result<Vec<(RepositoryName, PathBuf)>> {
    let mut out = Vec::new();
    collect_repository_files(root, root, &mut out)?;
    out.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
    Ok(out)
}

fn collect_repository_files(
    dir: &Path,
    root: &Path,
    out: &mut Vec<(RepositoryName, PathBuf)>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::Operation(format!("could not read {}: {error}", dir.display())));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", dir.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| Error::Operation(format!("could not stat {}: {e}", path.display())))?;
        if file_type.is_dir() {
            collect_repository_files(&path, root, out)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("repository.json") {
            let parent = path
                .parent()
                .ok_or_else(|| Error::Integrity("repository.json has no parent".to_string()))?;
            let relative = parent.strip_prefix(root).map_err(|_| {
                Error::Integrity(format!("repository path escapes the generation: {}", parent.display()))
            })?;
            let mut parts = Vec::new();
            for component in relative.components() {
                let text = component
                    .as_os_str()
                    .to_str()
                    .ok_or_else(|| Error::Integrity("repository path is not UTF-8".to_string()))?;
                parts.push(text);
            }
            let name = RepositoryName::parse(&parts.join("/"))
                .map_err(|error| Error::Integrity(format!("invalid repository directory: {error}")))?;
            out.push((name, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stuff_oci::Tag;

    fn fixture_state() -> (tempfile::TempDir, State) {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().to_path_buf());
        let layout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oci/layout");
        let repository = RepositoryName::parse("example/app").unwrap();
        crate::import::import_layout(&state, &layout, &repository, Some(&Tag::parse("stable").unwrap()))
            .unwrap();
        (dir, state)
    }

    fn publish_fixture(state: &State, site: &Path) -> PublishSummary {
        publish(state, site, DEFAULT_PUBLIC_URL).unwrap()
    }

    fn tree(root: &Path) -> FxHashMap<String, Vec<u8>> {
        let mut out = FxHashMap::default();
        collect_files(root, root, &mut out).unwrap();
        out
    }

    #[test]
    fn builds_the_exact_site_tree_and_schema_four_catalog() {
        let (state_dir, state) = fixture_state();
        let site_dir = tempfile::tempdir().unwrap();
        let summary = publish_fixture(&state, site_dir.path());
        assert!(!summary.no_op);
        assert_eq!(summary.generation.len(), 24);
        assert_eq!(summary.repositories, 1);
        assert_eq!(summary.manifests, 4);
        assert_eq!(summary.blobs, 6);
        assert_eq!(summary.referrers, 1);

        let generation = std::fs::read_link(site_dir.path().join("current")).unwrap();
        assert_eq!(generation, PathBuf::from(format!("generations/{}", summary.generation)));
        let generation_dir = site_dir.path().join(&generation);

        let mut expected = vec![
            "blobs.json".to_string(),
            "catalog.json".to_string(),
            "index.html".to_string(),
            "llms.txt".to_string(),
            "repositories/example/app/repository.json".to_string(),
            "repositories/example/app/referrers/sha256/576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4.json".to_string(),
        ];
        let actual = tree(&generation_dir);
        expected.sort();
        let mut keys: Vec<_> = actual.keys().cloned().collect();
        keys.sort_unstable();
        assert_eq!(keys, expected);

        let mut shared = Vec::new();
        for digest in summary_digests(&state) {
            shared.push(format!("oci/manifests/sha256/{}", digest.hex()));
        }
        let site_tree = tree(site_dir.path());
        for path in &shared {
            assert!(site_tree.contains_key(path), "{path}");
        }
        for digest in state.read_blobs().unwrap() {
            assert!(site_tree.contains_key(&format!("blobs/sha256/{}", digest.hex())));
        }

        let catalog = stuff_catalog::parse_catalog_json(
            &String::from_utf8(actual["catalog.json"].clone()).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog.schema_version, 4);
        assert_eq!(catalog.registry, stuff_catalog::Registry::Oci);
        assert_eq!(catalog.generation, summary.generation);
        assert_eq!(catalog.packages.len(), 1);
        assert_eq!(catalog.packages[0].name, "example/app");
        assert_eq!(catalog.packages[0].latest, "stable");
        assert_eq!(catalog.packages[0].versions[0].version, "stable");
        assert!(String::from_utf8(actual["index.html"].clone()).unwrap().contains("example/app"));
        assert!(String::from_utf8(actual["llms.txt"].clone()).unwrap().contains(DEFAULT_PUBLIC_URL));
        drop(state_dir);
    }

    fn summary_digests(state: &State) -> Vec<Digest> {
        let view = state
            .read_repository(&RepositoryName::parse("example/app").unwrap())
            .unwrap()
            .unwrap();
        view.manifests.keys().cloned().collect()
    }




}
