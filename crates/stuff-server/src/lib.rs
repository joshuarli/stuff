#![forbid(unsafe_code)]

pub mod config;
pub mod serve;

pub use config::{Config, Route};
pub use serve::{etag_for, map_request, respond, FileResponse, Mapped, ResponseBody};
