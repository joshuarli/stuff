#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::MirrorOptions;

pub fn run(_options: &MirrorOptions) -> Result<()> {
    Err(Error::Operation(
        "mirror resolve is not implemented yet; this build cannot fetch or pin graphs".to_string(),
    ))
}
