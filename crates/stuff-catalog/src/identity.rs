#![forbid(unsafe_code)]

use crate::{Catalog, render_catalog_json, render_html, render_llms_txt};
use sha2::{Digest, Sha256};

pub const FINGERPRINT_GENERATION: &str = "fingerprint";

pub fn catalog_fingerprint(catalog: &Catalog) -> String {
    let mut copy = catalog.clone();
    copy.generation = FINGERPRINT_GENERATION.to_string();
    let mut hasher = Sha256::new();
    hasher.update(render_catalog_json(&copy).as_bytes());
    hasher.update(b"\0");
    hasher.update(render_html(&copy).as_bytes());
    hasher.update(b"\0");
    hasher.update(render_llms_txt(&copy).as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CargoInput, CargoSource, cargo_catalog};

    fn catalog(version: &str) -> Catalog {
        cargo_catalog(
            "generation-a",
            vec![CargoInput {
                name: "foo".to_string(),
                version: version.to_string(),
                yanked: false,
                source: CargoSource::Native,
            }],
        )
        .unwrap()
    }

    #[test]
    fn fingerprint_ignores_generation_but_covers_rendered_content() {
        let first = catalog("1.0.0");
        let second = catalog("1.0.1");
        assert_eq!(catalog_fingerprint(&first), catalog_fingerprint(&first));
        assert_ne!(catalog_fingerprint(&first), catalog_fingerprint(&second));
    }
}
