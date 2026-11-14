#![forbid(unsafe_code)]

use crate::{Catalog, render_catalog_json, render_html, render_llms_txt};
use sha2::{Digest, Sha256};

fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

pub const FINGERPRINT_GENERATION: &str = "fingerprint";

fn fingerprint_of(catalog_json: &str, html: &str, llms_txt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(catalog_json.as_bytes());
    hasher.update(b"\0");
    hasher.update(html.as_bytes());
    hasher.update(b"\0");
    hasher.update(llms_txt.as_bytes());
    lower_hex(hasher.finalize())
}

pub fn catalog_fingerprint(catalog: &Catalog, origin: &str) -> String {
    let mut copy = catalog.clone();
    copy.generation = FINGERPRINT_GENERATION.to_string();
    fingerprint_of(
        &render_catalog_json(&copy),
        &render_html(&copy),
        &render_llms_txt(&copy, origin),
    )
}

/// The fingerprint a generation was sealed with, rebuilt from the catalog
/// files stored in it, so it stays checkable after the renderer changes.
pub fn stored_catalog_fingerprint(generation: &str, catalog_json: &str, html: &str, llms_txt: &str) -> String {
    let unbind = |text: &str| text.replace(generation, FINGERPRINT_GENERATION);
    fingerprint_of(&unbind(catalog_json), &unbind(html), &unbind(llms_txt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llms::DEFAULT_CARGO_ORIGIN;
    use crate::model::{CargoInput, CargoSource, Dependencies, cargo_catalog};

    fn catalog(version: &str) -> Catalog {
        cargo_catalog(
            "generation-a",
            vec![CargoInput {
                name: "foo".to_string(),
                version: version.to_string(),
                yanked: false,
                source: CargoSource::Native,
                dependencies: Dependencies::default(),
            }],
        )
        .unwrap()
    }

    #[test]
    fn stored_files_reproduce_the_sealed_fingerprint() {
        let catalog = catalog("1.0.0");
        let stored = stored_catalog_fingerprint(
            &catalog.generation,
            &render_catalog_json(&catalog),
            &render_html(&catalog),
            &render_llms_txt(&catalog, DEFAULT_CARGO_ORIGIN),
        );
        assert_eq!(stored, catalog_fingerprint(&catalog, DEFAULT_CARGO_ORIGIN));
    }

    #[test]
    fn stored_files_reproduce_the_sealed_fingerprint_for_full_length_ids() {
        let mut catalog = catalog("1.0.0");
        catalog.generation = "0fd089545b93e4a6294d8cd2".to_string();
        let stored = stored_catalog_fingerprint(
            &catalog.generation,
            &render_catalog_json(&catalog),
            &render_html(&catalog),
            &render_llms_txt(&catalog, DEFAULT_CARGO_ORIGIN),
        );
        assert_eq!(stored, catalog_fingerprint(&catalog, DEFAULT_CARGO_ORIGIN));
    }

    #[test]
    fn fingerprint_ignores_generation_but_covers_rendered_content() {
        let first = catalog("1.0.0");
        let second = catalog("1.0.1");
        assert_eq!(
            catalog_fingerprint(&first, DEFAULT_CARGO_ORIGIN),
            catalog_fingerprint(&first, DEFAULT_CARGO_ORIGIN)
        );
        assert_ne!(
            catalog_fingerprint(&first, DEFAULT_CARGO_ORIGIN),
            catalog_fingerprint(&second, DEFAULT_CARGO_ORIGIN)
        );
        assert_ne!(
            catalog_fingerprint(&first, DEFAULT_CARGO_ORIGIN),
            catalog_fingerprint(&first, "https://cargo.example.test")
        );
    }
}
