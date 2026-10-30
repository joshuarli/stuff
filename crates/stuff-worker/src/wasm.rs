use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn map_asset_json(routes_json: &str, host: &str, raw_path: &str) -> String {
    crate::api::map_asset_json(routes_json, host, raw_path)
}

#[wasm_bindgen]
pub fn resolve_pointer_json(pointer_key: &str, pointer_text: &str, rel_path: &str) -> String {
    crate::api::resolve_pointer_json(pointer_key, pointer_text, rel_path)
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
