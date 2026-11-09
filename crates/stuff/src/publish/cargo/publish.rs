use std::fs;
use std::path::{Path, PathBuf};

use stuff_store::{HashAlg, Site, sync_dir, sync_immutable_file, write_file_durable};

use super::render::{CargoUrls, Rendered, render, sha256_hex};
use super::store::Store;
use super::{Rejection, RejectionKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOutcome {
    pub generation_id: String,
    pub is_noop: bool,
}

pub struct Publisher<'a> {
    store: &'a Store,
    site_dir: PathBuf,
    urls: CargoUrls,
    auth_required: bool,
}

impl<'a> Publisher<'a> {
    pub fn new(
        store: &'a Store,
        site_dir: impl Into<PathBuf>,
        urls: CargoUrls,
        auth_required: bool,
    ) -> Self {
        Self {
            store,
            site_dir: site_dir.into(),
            urls,
            auth_required,
        }
    }

    pub fn publish(&self) -> Result<PublishOutcome, Rejection> {
        let site = Site::new(self.site_dir.clone());
        site.recover_staging().map_err(store_error)?;
        site.check_no_symlinks_in_path().map_err(store_error)?;
        site.ensure_layout().map_err(store_error)?;
        let blobs_dir = self.site_dir.join("blobs");
        fs::create_dir_all(&blobs_dir).map_err(|error| {
            operation(format!(
                "could not create {}: {error}",
                blobs_dir.display()
            ))
        })?;
        sync_dir(&blobs_dir).map_err(store_error)?;

        let native = self.store.load_native_records()?;
        let upstream = self.store.load_upstream_records()?;
        for checksum in native
            .iter()
            .map(|record| record.entry.cksum.as_str())
            .chain(upstream.iter().map(|record| record.checksum.as_str()))
        {
            sync_immutable_file(
                &self.store.blob_path(checksum),
                &blobs_dir.join(format!("{checksum}.crate")),
                checksum,
                HashAlg::Sha256,
            )
            .map_err(store_error)?;
        }

        let rendered = render(&native, &upstream, &self.urls, self.auth_required)?;
        let activation = site
            .publish(
                &rendered.generation_id,
                &|staging| build_generation(staging, &rendered),
                &|generation| generation_matches(generation, &rendered),
            )
            .map_err(store_error)?;
        Ok(PublishOutcome {
            generation_id: activation.generation_id,
            is_noop: activation.is_noop,
        })
    }
}

fn build_generation(staging: &Path, rendered: &Rendered) -> stuff_store::Result<()> {
    for ((view, relative), data) in &rendered.files {
        let target = if view.is_empty() {
            staging.join(relative)
        } else {
            staging.join(view).join(relative)
        };
        write_file_durable(&target, data)?;
    }
    Ok(())
}

fn generation_matches(generation: &Path, rendered: &Rendered) -> stuff_store::Result<bool> {
    for ((view, relative), expected) in &rendered.manifest {
        let path = if view.is_empty() {
            generation.join(relative)
        } else {
            generation.join(view).join(relative)
        };
        match fs::read(&path) {
            Ok(data) if &sha256_hex(&data) == expected => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn operation(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Operation, message)
}

fn store_error(error: stuff_store::Error) -> Rejection {
    use stuff_store::Error;
    match error {
        Error::Input(message) => Rejection::new(RejectionKind::Input, message),
        Error::Integrity(message) => Rejection::new(RejectionKind::Integrity, message),
        Error::Conflict(message) => Rejection::new(RejectionKind::Conflict, message),
        Error::Limit(message) => Rejection::new(RejectionKind::Limit, message),
        Error::Network(message) => Rejection::new(RejectionKind::Network, message),
        Error::Operation(message) => Rejection::new(RejectionKind::Operation, message),
    }
}
