#![forbid(unsafe_code)]

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    BlobUnknown,
    BlobUploadInvalid,
    BlobUploadUnknown,
    Conflict,
    Denied,
    DigestInvalid,
    ManifestBlobUnknown,
    ManifestInvalid,
    ManifestUnknown,
    ManifestUnverified,
    NameInvalid,
    NameUnknown,
    PaginationNumberInvalid,
    RangeInvalid,
    SizeInvalid,
    TagInvalid,
    Unauthorized,
    Unsupported,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BlobUnknown => "BLOB_UNKNOWN",
            ErrorCode::BlobUploadInvalid => "BLOB_UPLOAD_INVALID",
            ErrorCode::BlobUploadUnknown => "BLOB_UPLOAD_UNKNOWN",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::Denied => "DENIED",
            ErrorCode::DigestInvalid => "DIGEST_INVALID",
            ErrorCode::ManifestBlobUnknown => "MANIFEST_BLOB_UNKNOWN",
            ErrorCode::ManifestInvalid => "MANIFEST_INVALID",
            ErrorCode::ManifestUnknown => "MANIFEST_UNKNOWN",
            ErrorCode::ManifestUnverified => "MANIFEST_UNVERIFIED",
            ErrorCode::NameInvalid => "NAME_INVALID",
            ErrorCode::NameUnknown => "NAME_UNKNOWN",
            ErrorCode::PaginationNumberInvalid => "PAGINATION_NUMBER_INVALID",
            ErrorCode::RangeInvalid => "RANGE_INVALID",
            ErrorCode::SizeInvalid => "SIZE_INVALID",
            ErrorCode::TagInvalid => "TAG_INVALID",
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::Unsupported => "UNSUPPORTED",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            ErrorCode::BlobUnknown | ErrorCode::ManifestUnknown | ErrorCode::ManifestUnverified => 404,
            ErrorCode::BlobUploadInvalid
            | ErrorCode::DigestInvalid
            | ErrorCode::ManifestBlobUnknown
            | ErrorCode::ManifestInvalid
            | ErrorCode::NameInvalid
            | ErrorCode::PaginationNumberInvalid
            | ErrorCode::SizeInvalid
            | ErrorCode::TagInvalid => 400,
            ErrorCode::Denied => 403,
            ErrorCode::Conflict => 409,
            ErrorCode::RangeInvalid => 416,
            ErrorCode::Unauthorized => 401,
            ErrorCode::Unsupported => 405,
            ErrorCode::BlobUploadUnknown | ErrorCode::NameUnknown => 404,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<String>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn status(&self) -> u16 {
        self.code.status()
    }

    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"errors\":[{\"code\":");
        out.push_str(&crate::json::quoted(self.code.as_str()));
        out.push_str(",\"message\":");
        out.push_str(&crate::json::quoted(&self.message));
        if let Some(detail) = &self.detail {
            out.push_str(",\"detail\":");
            out.push_str(&crate::json::quoted(detail));
        }
        out.push_str("}]}");
        out
    }

    pub fn digest_invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::DigestInvalid, "invalid digest").with_detail(detail)
    }

    pub fn manifest_invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::ManifestInvalid, "invalid manifest").with_detail(detail)
    }

    pub fn name_invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::NameInvalid, "invalid repository name").with_detail(detail)
    }

    pub fn tag_invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::TagInvalid, "invalid tag").with_detail(detail)
    }

    pub fn name_unknown(name: &str) -> Self {
        Self::new(ErrorCode::NameUnknown, "repository name not known to registry")
            .with_detail(name)
    }

    pub fn manifest_unknown(reference: &str) -> Self {
        Self::new(ErrorCode::ManifestUnknown, "manifest unknown").with_detail(reference)
    }

    pub fn blob_unknown(digest: &str) -> Self {
        Self::new(ErrorCode::BlobUnknown, "blob unknown to registry").with_detail(digest)
    }

    pub fn pagination_number_invalid(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::PaginationNumberInvalid, "invalid pagination number")
            .with_detail(detail)
    }

    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unsupported, "operation unsupported").with_detail(detail)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_oci_error_envelope() {
        let error = Error::new(ErrorCode::ManifestUnknown, "manifest unknown").with_detail("sha256:0");
        assert_eq!(
            error.to_json(),
            "{\"errors\":[{\"code\":\"MANIFEST_UNKNOWN\",\"message\":\"manifest unknown\",\"detail\":\"sha256:0\"}]}"
        );
        assert_eq!(ErrorCode::NameInvalid.status(), 400);
        assert_eq!(ErrorCode::NameUnknown.status(), 404);
        assert_eq!(ErrorCode::Unsupported.status(), 405);
        assert_eq!(ErrorCode::Unauthorized.status(), 401);
        assert_eq!(ErrorCode::RangeInvalid.status(), 416);
    }
}
