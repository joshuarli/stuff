#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::MirrorOptions;

pub fn run(_options: &MirrorOptions) -> Result<()> {
    Err(Error::Operation(
        "mirror sync is not implemented yet; this build cannot acquire or apply the pinned graphs".to_string(),
    ))
}
