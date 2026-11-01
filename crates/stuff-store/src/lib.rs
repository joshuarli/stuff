#![forbid(unsafe_code)]

pub mod error;
pub mod generation;

#[cfg(unix)]
pub mod blob;
#[cfg(unix)]
pub mod fsync;
#[cfg(unix)]
pub mod hash;
#[cfg(unix)]
pub mod lock;
#[cfg(unix)]
pub mod site;

#[cfg(unix)]
pub use blob::sync_immutable_file;
pub use error::{Error, Result};
pub use generation::parse_generation_id;
#[cfg(unix)]
pub use fsync::{sync_dir, sync_tree, tmp_sibling, write_file_durable};
#[cfg(unix)]
pub use hash::{stream_file_hash, HashAlg};
#[cfg(unix)]
pub use lock::{FileLock, LockError};
#[cfg(unix)]
pub use site::{Activation, Site, Staged};
