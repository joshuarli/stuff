#![forbid(unsafe_code)]

pub mod map;

#[cfg(target_arch = "wasm32")]
pub mod worker;

pub use map::{
    map_asset, object_key_for, parse_pointer, pointer_key, AssetKey, MappedAsset, RegistryRoute,
    RouteKind, WorkerConfig,
};
