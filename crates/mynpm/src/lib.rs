#![forbid(unsafe_code)]

pub mod acquire;
pub mod archive;
pub mod cli;
pub mod config;
pub mod doctor;
pub mod error;
pub mod first_party;
pub mod fetch_budget;
pub mod inventory;
pub mod inventory_store;
pub mod limits;
pub mod lock_npm;
pub mod lock_pnpm;
pub mod packument;
pub mod process;
pub mod project_snapshot;
pub mod publish;
pub mod resolve;
pub mod validate;
pub mod value;

pub(crate) fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}
