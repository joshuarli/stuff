#![forbid(unsafe_code)]

pub mod cache;
pub mod condition;
pub mod mime;
pub mod npm;
pub mod route;

pub use cache::{cache_policy_for, CachePolicy, RouteKind};
pub use condition::{evaluate_preconditions, parse_range, Precondition, RangeError, RangeRequest};
pub use mime::content_type_for;
pub use npm::{packument_tarball_digest, parse_npm_tarball_path, NpmTarballPath};
pub use route::{
    is_immutable_asset, plan_route, resolve_path, shared_store_path, PathError, ResolvedPath,
    RoutePlan, RouteScope, GENERATION_CATALOG,
};
