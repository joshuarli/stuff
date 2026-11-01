#![forbid(unsafe_code)]

pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
pub const DOCKER_SCHEMA1: &str = "application/vnd.docker.distribution.manifest.v1+json";
pub const DOCKER_SCHEMA1_SIGNED: &str = "application/vnd.docker.distribution.manifest.v1+prettyjws";
pub const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
pub const DOCKER_CONFIG: &str = "application/vnd.docker.container.image.v1+json";
pub const OCI_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
pub const OCI_LAYER_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
pub const DOCKER_LAYER_GZIP: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

pub fn is_manifest(media_type: &str) -> bool {
    media_type == OCI_MANIFEST || media_type == DOCKER_MANIFEST
}

pub fn is_index(media_type: &str) -> bool {
    media_type == OCI_INDEX || media_type == DOCKER_LIST
}

pub fn is_manifest_or_index(media_type: &str) -> bool {
    is_manifest(media_type) || is_index(media_type)
}

pub fn is_foreign_layer(media_type: &str) -> bool {
    media_type.starts_with("application/vnd.docker.image.rootfs.foreign.")
        || media_type.starts_with("application/vnd.oci.image.layer.nondistributable.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_media_types() {
        assert!(is_manifest(OCI_MANIFEST));
        assert!(is_manifest(DOCKER_MANIFEST));
        assert!(is_index(OCI_INDEX));
        assert!(is_index(DOCKER_LIST));
        assert!(!is_index(OCI_MANIFEST));
        assert!(!is_manifest_or_index(DOCKER_SCHEMA1));
        assert!(is_foreign_layer("application/vnd.docker.image.rootfs.foreign.diff.tar.gzip"));
        assert!(is_foreign_layer("application/vnd.oci.image.layer.nondistributable.v1.tar+gzip"));
        assert!(!is_foreign_layer(OCI_LAYER_GZIP));
    }
}
