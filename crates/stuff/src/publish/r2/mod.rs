#![forbid(unsafe_code)]

pub mod client;
pub mod credentials;
pub mod guard;
pub mod manifest;
pub mod multipart;
pub mod promote;
pub mod pull;
pub mod put;
pub mod sigv4;
pub mod sync;
pub mod xml;

pub use client::{Credentials, ObjectMeta, Precondition, R2Client, RawRequest, RawResponse};
pub use manifest::{Manifest, ManifestObject};
pub use multipart::{MultipartUpload, PART_SIZE};
pub use put::{content_type, UploadOptions};
pub use sigv4::{SignedHeaders, SigningMaterial, SignRequest};
pub use xml::{ListPage, ListedObject, S3Error};
