#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::MirrorOptions;

pub fn run(_options: &MirrorOptions) -> Result<()> {
    Err(Error::Operation(
        "mirror plan is not implemented yet".to_string(),
    ))
}
