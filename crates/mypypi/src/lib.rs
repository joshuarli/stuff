#![forbid(unsafe_code)]

pub mod acquire;
pub mod add;
pub mod build;
pub mod catalog;
pub mod cli;
pub mod config;
pub mod error;
pub mod index;
pub mod json;
pub mod models;
pub mod platforms;
pub mod planner;
pub mod process_policy;
pub mod publish;
pub mod store;
pub mod validate;
pub mod wheel_metadata;

pub(crate) fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}
