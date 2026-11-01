#![forbid(unsafe_code)]

mod digest;
mod error;
mod json;
mod manifest;
mod media;
mod reference;
mod referrers;
mod request;
mod response;
mod session;
mod view;

pub use digest::Digest;
pub use error::{Error, ErrorCode};
pub use manifest::{validate_manifest_push, Image, Index, Manifest, Platform, MAX_MANIFEST_BYTES};
pub use media::{
    is_foreign_layer, is_index, is_manifest, is_manifest_or_index, DOCKER_CONFIG, DOCKER_LAYER_GZIP,
    DOCKER_LIST, DOCKER_MANIFEST, OCI_CONFIG, OCI_INDEX, OCI_LAYER_GZIP, OCI_LAYER_ZSTD,
    OCI_MANIFEST,
};
pub use reference::{Reference, RepositoryName, Tag, MAX_REPOSITORY_NAME, MAX_TAG};
pub use referrers::{filter_referrers, parse_referrers, referrer_descriptor, render_referrers};
pub use request::{
    parse_request, parse_write_request, Method, Pagination, Parsed, ParsedWrite, Request,
    WriteMethod, WriteRequest,
};
pub use response::{
    plan_auth_required, plan_blob, plan_error, plan_manifest, plan_manifest_put,
    plan_method_not_allowed, plan_ping, plan_referrers, plan_tags, plan_upload_cancel,
    plan_upload_complete, plan_upload_mounted, plan_upload_patch, plan_upload_start,
    plan_upload_status, Body, RequestHeaders, ResponsePlan,
};
pub use session::{parse_basic_auth, SessionToken, DEFAULT_SESSION_TTL_SECS};
pub use view::{
    parse_blobs_json, parse_repository_json, render_blobs_json, render_repository_json,
    RepositoryView, BLOBS_SCHEMA_VERSION, REPOSITORY_SCHEMA_VERSION,
};

pub use manifest::{parse_image, Descriptor};
