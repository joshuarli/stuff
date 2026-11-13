#![forbid(unsafe_code)]

pub mod cargo;
pub mod config;
pub mod environment;
pub mod inventory;
pub mod mirror;
pub mod npm;
pub mod npm_first_party;
pub mod oci;
pub mod output;
pub mod paths;
pub mod pypi;
pub mod pypi_first_party;
pub mod r2;
pub mod republish;
pub mod rollback;
pub mod snapshot;
pub mod state;
pub(crate) mod staging;
pub mod transaction;

pub use environment::Environment;

pub(crate) fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registry {
    Cargo,
    Npm,
    Pypi,
    Oci,
    Mirror,
}

impl Registry {
    pub fn as_str(self) -> &'static str {
        match self {
            Registry::Cargo => "cargo",
            Registry::Npm => "npm",
            Registry::Pypi => "pypi",
            Registry::Oci => "oci",
            Registry::Mirror => "mirror",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "cargo" => Some(Registry::Cargo),
            "npm" => Some(Registry::Npm),
            "pypi" => Some(Registry::Pypi),
            "oci" => Some(Registry::Oci),
            "mirror" => Some(Registry::Mirror),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub dry_run: bool,
    pub json: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoPublish {
    pub packages: Vec<String>,
    pub manifest_path: Option<String>,
    pub registry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmPublish {
    pub lockfile: String,
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorPublish {
    pub manifest: Option<String>,
    pub allow_drops: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PypiSpec {
    pub name: String,
    pub version: String,
}

impl PypiSpec {
    pub fn parse(text: &str) -> Option<Self> {
        let (name, version) = text.split_once("==")?;
        let invalid = |value: &str| {
            value.is_empty() || value.contains(|c: char| c == '=' || c.is_whitespace())
        };
        if invalid(name) || invalid(version) {
            return None;
        }
        Some(PypiSpec { name: name.to_string(), version: version.to_string() })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PypiPublish {
    pub specs: Vec<PypiSpec>,
    pub allow_sdist: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciImage {
    pub repository: String,
    pub reference: String,
}

impl OciImage {
    pub fn parse(text: &str) -> Option<Self> {
        let (repository, reference) = text.split_once('=')?;
        let invalid = |value: &str| {
            value.is_empty() || value.contains(|c: char| c == '=' || c.is_whitespace())
        };
        if invalid(repository) || invalid(reference) {
            return None;
        }
        Some(OciImage { repository: repository.to_string(), reference: reference.to_string() })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OciSource {
    Images(Vec<OciImage>),
    Layout { dir: String, repository: String, tag: Option<String> },
    Mirror { inventory: String, lock: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishRequest {
    Cargo(CargoPublish),
    CargoLockfiles(Vec<String>),
    Npm(NpmPublish),
    NpmTarballs(Vec<String>),
    Mirror(MirrorPublish),
    Pypi(PypiPublish),
    PypiWheels(Vec<String>),
    Oci(OciSource),
}

impl PublishRequest {
    pub fn registry(&self) -> Registry {
        match self {
            PublishRequest::Cargo(_) | PublishRequest::CargoLockfiles(_) => Registry::Cargo,
            PublishRequest::Npm(_) | PublishRequest::NpmTarballs(_) => Registry::Npm,
            PublishRequest::Mirror(_) => Registry::Mirror,
            PublishRequest::Pypi(_) | PublishRequest::PypiWheels(_) => Registry::Pypi,
            PublishRequest::Oci(_) => Registry::Oci,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Publish {
        request: PublishRequest,
        options: Options,
    },
    Republish {
        registry: Registry,
        allow_drops: bool,
        options: Options,
        manifest: Option<String>,
    },
    Rollback {
        registry: Registry,
        generation: String,
        options: Options,
    },
}

impl Request {
    pub fn command(&self) -> &'static str {
        match self {
            Request::Publish { .. } => "publish",
            Request::Republish { .. } => "republish",
            Request::Rollback { .. } => "rollback",
        }
    }

    pub fn registry(&self) -> Registry {
        match self {
            Request::Publish { request, .. } => request.registry(),
            Request::Republish { registry, .. } | Request::Rollback { registry, .. } => *registry,
        }
    }

    pub fn options(&self) -> Options {
        match self {
            Request::Publish { options, .. }
            | Request::Republish { options, .. }
            | Request::Rollback { options, .. } => *options,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Published,
    Unchanged,
    Conflict,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Published => "published",
            Status::Unchanged => "unchanged",
            Status::Conflict => "conflict",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub registry: Registry,
    pub status: Status,
    pub generation: String,
    pub packages: Vec<String>,
    pub dry_run: bool,
    pub uploaded_objects: usize,
    pub uploaded_bytes: u64,
    pub skipped_objects: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success,
    Usage,
    Conflict,
    Integrity,
    Transport,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        match self {
            ExitCode::Success => 0,
            ExitCode::Usage => 2,
            ExitCode::Conflict => 3,
            ExitCode::Integrity => 4,
            ExitCode::Transport => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishError {
    Unavailable { command: &'static str, registry: &'static str },
    Usage(String),
    Config(String),
    Conflict(String),
    Integrity(String),
    Transport(String),
}

impl PublishError {
    pub fn classification(&self) -> ExitCode {
        match self {
            PublishError::Unavailable { .. } | PublishError::Usage(_) | PublishError::Config(_) => {
                ExitCode::Usage
            }
            PublishError::Conflict(_) => ExitCode::Conflict,
            PublishError::Integrity(_) => ExitCode::Integrity,
            PublishError::Transport(_) => ExitCode::Transport,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.classification().code()
    }
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Unavailable { command, registry } => {
                write!(f, "{command} {registry} is not available in this build")
            }
            PublishError::Usage(detail) => write!(f, "{detail}"),
            PublishError::Config(detail) => write!(f, "config: {detail}"),
            PublishError::Conflict(detail) => write!(f, "conflict: {detail}"),
            PublishError::Integrity(detail) => write!(f, "integrity: {detail}"),
            PublishError::Transport(detail) => write!(f, "transport: {detail}"),
        }
    }
}

impl std::error::Error for PublishError {}

pub fn publish_lock_for_bucket(
    environment: &Environment,
    bucket: &str,
) -> Result<std::path::PathBuf, PublishError> {
    let paths = paths::Paths::from_environment(environment);
    let config = config::Config::load(&paths.config_file())?;
    let registry = config.registry_for_bucket(bucket).ok_or_else(|| {
        PublishError::Config(format!(
            "bucket {bucket} is not configured for any registry in {}",
            paths.config_file().display()
        ))
    })?;
    Ok(paths.publish_lock(registry))
}

pub fn run(request: Request, environment: &Environment) -> Result<Outcome, PublishError> {
    match request {
        Request::Publish {
            request: PublishRequest::Cargo(cargo),
            options,
        } => cargo::run::publish(&cargo, options, environment),
        Request::Publish {
            request: PublishRequest::CargoLockfiles(lockfiles),
            options,
        } => cargo::mirror::publish(&lockfiles, options, environment),
        Request::Publish {
            request: PublishRequest::Npm(request),
            options,
        } => npm::publish(&request, options, environment),
        Request::Publish {
            request: PublishRequest::NpmTarballs(tarballs),
            options,
        } => npm_first_party::publish(&tarballs, options, environment),
        Request::Publish {
            request: PublishRequest::Mirror(request),
            options,
        } => mirror::publish(&request, options, environment),
        Request::Publish {
            request: PublishRequest::Oci(source),
            options,
        } => oci::publish(&source, &options, environment),
        Request::Publish {
            request: PublishRequest::Pypi(pypi),
            options,
        } => pypi::run(&pypi, &options, environment),
        Request::Publish {
            request: PublishRequest::PypiWheels(wheels),
            options,
        } => pypi_first_party::publish(&wheels, &options, environment),
        Request::Republish {
            registry,
            allow_drops,
            options,
            manifest,
        } => republish::republish(registry, allow_drops, manifest, &options, environment),
        Request::Rollback {
            registry,
            generation,
            options,
        } => rollback::rollback(registry, &generation, &options, environment),
    }
}

const EXIT_CODES: &str = "\
Exit codes:
  0  success or no-op
  2  usage or configuration error
  3  conflict or stale generation
  4  integrity or verification failure
  5  transport or network failure
";

pub fn help_text(command: &str) -> Option<String> {
    let body = match command {
        "publish" => concat!(
            "stuff publish\n\n",
            "Usage:\n",
            "  stuff publish cargo (-p NAME [-p NAME...] [--manifest-path PATH] [--registry NAME] |\n",
            "    --lockfile PATH [--lockfile PATH...]) [--dry-run] [--json] [--verbose]\n",
            "    --lockfile mirrors the crates.io packages a Cargo.lock pins\n",
            "  stuff publish npm (--lockfile PATH --project ID | --tarball PATH [--tarball PATH...])\n",
            "    [--dry-run] [--json] [--verbose]\n",
            "    --tarball publishes first-party packages packed by npm pack or pnpm pack\n",
            "  stuff publish mirror [--manifest PATH] [--allow-drops] [--dry-run] [--json] [--verbose]\n",
            "    --manifest reads the mirror manifest (default mirror/manifest.toml)\n",
            "    rebuilds the mirror from the pinned upstream artifacts\n",
            "    --allow-drops replaces pinned artifact versions, which the drop guard counts as removals\n",
            "  stuff publish pypi (SPEC [SPEC...] [--allow-sdist] | --wheel PATH [--wheel PATH...])\n",
            "    [--dry-run] [--json] [--verbose]\n",
            "    SPEC is name==version\n",
            "    --wheel publishes first-party pure-Python (none-any) wheels built by uv build or pip wheel\n",
            "  stuff publish oci (--image REPO=REF [--image REPO=REF...] |\n",
            "    --layout DIR --repository NAME [--tag TAG] |\n",
            "    --inventory FILE --lock FILE)\n",
            "    [--dry-run] [--json] [--verbose]\n",
            "    --inventory/--lock mirrors a mirror file's pinned entries and manages\n",
            "    the aliases it declares\n\n",
        ),
        "republish" => concat!(
            "stuff republish\n\n",
            "Usage:\n",
            "  stuff republish cargo|npm|pypi|oci|mirror [--allow-drops]\n",
            "    [--dry-run] [--json] [--verbose]\n\n",
        ),
        "rollback" => concat!(
            "stuff rollback\n\n",
            "Usage:\n",
            "  stuff rollback cargo|npm|pypi|oci|mirror GENERATION\n",
            "    [--dry-run] [--json] [--verbose]\n\n",
        ),
        _ => return None,
    };
    Some(format!("{body}{EXIT_CODES}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(registry: Registry) -> Request {
        Request::Republish {
            registry,
            allow_drops: false,
            options: Options::default(),
            manifest: None,
        }
    }

    #[test]
    fn exit_codes_are_the_closed_p8_table() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::Usage.code(), 2);
        assert_eq!(ExitCode::Conflict.code(), 3);
        assert_eq!(ExitCode::Integrity.code(), 4);
        assert_eq!(ExitCode::Transport.code(), 5);
    }

    #[test]
    fn every_publish_error_maps_to_one_exit_code() {
        assert_eq!(PublishError::Usage(String::new()).exit_code(), 2);
        assert_eq!(PublishError::Config(String::new()).exit_code(), 2);
        assert_eq!(PublishError::Conflict(String::new()).exit_code(), 3);
        assert_eq!(PublishError::Integrity(String::new()).exit_code(), 4);
        assert_eq!(PublishError::Transport(String::new()).exit_code(), 5);
        assert_eq!(
            PublishError::Unavailable { command: "publish", registry: "cargo" }.exit_code(),
            2
        );
    }

    #[test]
    fn republish_dispatches_to_the_registry_and_reads_its_config() {
        for registry in [Registry::Cargo, Registry::Npm, Registry::Pypi, Registry::Oci] {
            let request = request(registry);
            assert_eq!(request.command(), "republish");
            assert_eq!(request.registry(), registry);
        }
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("config.json"), "{not json").unwrap();
        let environment = Environment::from_pairs([
            ("STUFF_CONFIG_HOME", work.path().display().to_string()),
            ("R2_S3_ENDPOINT", "http://127.0.0.1:9".to_string()),
            ("R2_S3_ACCESS_KEY_ID", "test".to_string()),
            ("R2_S3_SECRET_ACCESS_KEY", "test".to_string()),
        ]);
        let error = run(request(Registry::Cargo), &environment).unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("config"), "{error}");
    }

    #[test]
    fn pypi_specs_reject_malformed_shapes() {
        assert_eq!(
            PypiSpec::parse("pkg==1.2.3"),
            Some(PypiSpec { name: "pkg".into(), version: "1.2.3".into() })
        );
        assert!(PypiSpec::parse("pkg").is_none());
        assert!(PypiSpec::parse("pkg==").is_none());
        assert!(PypiSpec::parse("==1.0").is_none());
        assert!(PypiSpec::parse("pkg==1==2").is_none());
        assert!(PypiSpec::parse("pkg = 1").is_none());
    }

    #[test]
    fn oci_images_reject_malformed_shapes() {
        assert_eq!(
            OciImage::parse("repo/name=latest"),
            Some(OciImage { repository: "repo/name".into(), reference: "latest".into() })
        );
        assert!(OciImage::parse("repo/name").is_none());
        assert!(OciImage::parse("=latest").is_none());
        assert!(OciImage::parse("repo/name=").is_none());
        assert!(OciImage::parse("repo/name=sha256:abc=def").is_none());
    }

    #[test]
    fn help_documents_the_grammar_and_exit_codes() {
        let help = help_text("publish").unwrap();
        for needle in [
            "publish cargo",
            "publish npm",
            "publish pypi",
            "publish oci",
            "publish mirror",
            "--dry-run",
            "--json",
            "Exit codes:",
            "2  usage or configuration error",
            "5  transport or network failure",
        ] {
            assert!(help.contains(needle), "missing {needle} in {help}");
        }
        assert!(help_text("ls").is_none());
    }
}
