#![forbid(unsafe_code)]

pub mod cache;
pub mod condition;
pub mod mime;
pub mod route;

pub use cache::{cache_policy_for, CachePolicy, RouteKind};
pub use condition::{evaluate_preconditions, parse_range, Precondition, RangeError, RangeRequest};
pub use mime::content_type_for;
pub use route::{resolve_path, PathError, ResolvedPath};
