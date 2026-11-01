#![forbid(unsafe_code)]

pub mod api;
pub mod map;
pub mod oci;

#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use map::{
    map_asset, object_key_for, parse_pointer, pointer_key, AssetKey, MappedAsset, RegistryRoute,
    WorkerConfig,
};
