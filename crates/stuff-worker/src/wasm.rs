use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn generation_id_valid(text: &str) -> bool {
    stuff_store::parse_generation_id(text).is_some()
}

#[wasm_bindgen]
pub fn oci_request_json(method: &str, raw_target: &str) -> String {
    crate::oci::oci_request_json(method, raw_target)
}

#[wasm_bindgen]
pub fn oci_ping_json() -> String {
    crate::oci::oci_ping_json()
}

#[wasm_bindgen]
pub fn oci_manifest_json(repository: &str, repository_text: &str, reference: &str, head: bool) -> String {
    crate::oci::oci_manifest_json(repository, repository_text, reference, head)
}

#[wasm_bindgen]
pub fn oci_tags_json(repository: &str, repository_text: &str, n: Option<usize>, last: Option<String>) -> String {
    crate::oci::oci_tags_json(repository, repository_text, n, last.as_deref())
}

#[wasm_bindgen]
pub fn oci_referrers_json(
    repository: &str,
    subject: &str,
    referrers_text: &str,
    artifact_type: Option<String>,
    n: Option<usize>,
    last: Option<String>,
) -> String {
    crate::oci::oci_referrers_json(
        repository,
        subject,
        referrers_text,
        artifact_type.as_deref(),
        n,
        last.as_deref(),
    )
}

#[wasm_bindgen]
pub fn oci_blob_json(method: &str, digest: &str, size: u64, headers_json: &str) -> String {
    crate::oci::oci_blob_json(method, digest, size, headers_json)
}

#[wasm_bindgen]
pub fn oci_method_not_allowed_json() -> String {
    crate::oci::oci_method_not_allowed_json()
}

#[wasm_bindgen]
pub fn oci_error_json(code: &str, detail: &str) -> String {
    crate::oci::oci_error_json(code, detail)
}

#[wasm_bindgen]
pub fn oci_auth_challenge_json() -> String {
    crate::oci::oci_auth_challenge_json()
}

#[wasm_bindgen]
pub fn oci_manifest_put_json(repository: &str, digest: &str) -> String {
    crate::oci::oci_manifest_put_json(repository, digest)
}

#[wasm_bindgen]
pub fn oci_write_request_json(method: &str, target: &str) -> String {
    crate::oci_push::oci_write_request_json(method, target)
}

#[wasm_bindgen]
pub fn oci_auth_check(header: &str, token: &str) -> bool {
    crate::oci_push::oci_auth_check(header, token)
}

#[wasm_bindgen]
pub fn oci_session_json(
    repository: &str,
    writer_origin: &str,
    secret: &str,
    now: u64,
    upload_id: &str,
) -> String {
    crate::oci_push::oci_session_json(repository, writer_origin, secret, now, upload_id)
}

#[wasm_bindgen]
pub fn oci_mount_json(repository: &str, digest: &str) -> String {
    crate::oci_push::oci_mount_json(repository, digest)
}

#[wasm_bindgen]
pub fn oci_manifest_refs_json(body: &str) -> String {
    crate::oci_push::oci_manifest_refs_json(body)
}

#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn oci_generation_json(
    repository: &str,
    repositories_json: &str,
    blobs_text: &str,
    referrers_json: &str,
    manifest_body: &str,
    reference: &str,
    available_blobs_json: &str,
    available_manifests_json: &str,
    public_url: &str,
) -> String {
    crate::oci_push::oci_generation_json(
        repository,
        repositories_json,
        blobs_text,
        referrers_json,
        manifest_body,
        reference,
        available_blobs_json,
        available_manifests_json,
        public_url,
    )
}

#[wasm_bindgen]
pub fn map_asset_json(routes_json: &str, host: &str, raw_path: &str) -> String {
    crate::api::map_asset_json(routes_json, host, raw_path)
}

#[wasm_bindgen]
pub fn resolve_pointer_json(pointer_key: &str, pointer_text: &str, rel_path: &str) -> String {
    crate::api::resolve_pointer_json(pointer_key, pointer_text, rel_path)
}

#[wasm_bindgen]
pub fn resolve_npm_tarball_json(pointer_key: &str, packument_text: &str, version: &str) -> String {
    crate::api::resolve_npm_tarball_json(pointer_key, packument_text, version)
}

#[wasm_bindgen]
pub fn resolve_direct_alias_json(key_prefix: &str, key: &str) -> String {
    crate::api::resolve_direct_alias_json(key_prefix, key)
}

#[wasm_bindgen]
pub fn plan_object_response_json(
    route: &str,
    method: &str,
    key: &str,
    headers_json: &str,
    size: u64,
    etag: &str,
) -> String {
    crate::api::plan_object_response_json(route, method, key, headers_json, size, etag)
}

#[wasm_bindgen]
pub fn render_unified_index_json(catalogs_json: &str) -> String {
    crate::api::render_unified_index_json(catalogs_json)
}

#[wasm_bindgen]
pub fn render_unified_llms_txt_json(
    catalogs_json: &str,
    routes_json: &str,
    request_origin: &str,
) -> String {
    crate::api::render_unified_llms_txt_json(catalogs_json, routes_json, request_origin)
}
