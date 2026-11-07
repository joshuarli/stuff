#![forbid(unsafe_code)]

pub mod client;
pub mod credentials;
pub mod sigv4;
pub mod xml;

pub use client::{Credentials, ObjectMeta, Precondition, R2Client, RawRequest, RawResponse};
pub use sigv4::{SignedHeaders, SigningMaterial, SignRequest};
pub use xml::{ListPage, ListedObject, S3Error};
