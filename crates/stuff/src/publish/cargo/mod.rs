
pub mod adopt;
#[cfg(test)]
mod cli;
pub mod crate_file;
pub mod index;
pub mod mirror;
pub mod model;
pub mod names;
pub mod publish;
pub mod render;
pub mod requirements;
pub mod run;
pub mod semver;
pub mod server;
pub mod snapshot;
pub mod store;
pub mod wire;
pub mod wirejson;

pub use adopt::{AdoptSummary, AdoptedLine, NativeRecord, Record, UpstreamRecord, View, adopt_site};
pub use crate_file::{Limits, PackagedDep, PackagedManifest, ScanResult};
pub use index::build_entry;
pub use model::{
    DependencyKind, Features, IndexDep, IndexEntry, PublishDependency, PublishRequest,
};
pub use publish::{PublishOutcome, Publisher};
pub use render::{CargoUrls, Rendered};
pub use semver::Version;
pub use store::Store;

pub const PUBLISH_RESPONSE: &str =
    "{\"warnings\":{\"invalid_categories\":[],\"invalid_badges\":[],\"other\":[]}}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionKind {
    Input,
    Integrity,
    Conflict,
    Limit,
    Network,
    Operation,
}

impl RejectionKind {
    pub fn prefix(self) -> &'static str {
        match self {
            RejectionKind::Input => "input",
            RejectionKind::Integrity => "integrity",
            RejectionKind::Conflict => "conflict",
            RejectionKind::Limit => "limit",
            RejectionKind::Network => "network",
            RejectionKind::Operation => "operation",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            RejectionKind::Input => 400,
            RejectionKind::Integrity => 422,
            RejectionKind::Conflict => 409,
            RejectionKind::Limit => 413,
            RejectionKind::Network => 502,
            RejectionKind::Operation => 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub kind: RejectionKind,
    pub detail: String,
}

impl Rejection {
    pub fn new(kind: RejectionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            detail: format!("{}: {}", kind.prefix(), message.into()),
        }
    }

    pub fn status(&self) -> u16 {
        self.kind.status()
    }

    pub fn body(&self) -> String {
        wirejson::error_body(&self.detail)
    }
}

pub(crate) fn input(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Input, message)
}

pub(crate) fn integrity(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Integrity, message)
}

pub(crate) fn limit(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Limit, message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Accepted {
    pub entry: IndexEntry,
    pub index_line: String,
    pub shard: String,
    pub cksum: String,
}

pub fn accept(request: &PublishRequest, archive: &[u8]) -> Result<Accepted, Rejection> {
    accept_with_limits(request, archive, &Limits::default())
}

pub fn accept_with_limits(
    request: &PublishRequest,
    archive: &[u8],
    limits: &Limits,
) -> Result<Accepted, Rejection> {
    let scan = crate_file::scan(archive, &request.name, &request.vers, limits)?;
    let manifest = crate_file::parse_packaged_manifest(&scan.cargo_toml)?;
    let entry = index::build_entry(request, &manifest, &scan.checksum)?;
    let shard = names::shard_for_name(&names::lower_name(&entry.name)).ok_or_else(|| {
        integrity(format!("crate name is not ASCII: {}", entry.name))
    })?;
    let index_line = entry.index_line();
    Ok(Accepted {
        entry,
        index_line,
        shard,
        cksum: scan.checksum,
    })
}
