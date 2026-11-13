use std::path::{Path, PathBuf};
use std::process::Command;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn seed_inventory_path() -> PathBuf {
    PathBuf::from(ROOT).join("../../deploy/oci-mirror.toml")
}

fn seed_lock_path() -> PathBuf {
    PathBuf::from(ROOT).join("../../deploy/oci-mirror.lock")
}

fn seed_pins_path(name: &str) -> PathBuf {
    PathBuf::from(ROOT).join(format!("../../deploy/{name}"))
}

fn parse_inventory(text: &str) -> Result<myoci::mirror::Inventory, myoci::Error> {
    myoci::mirror::Inventory::parse(text)
}

fn parse_lock(text: &str) -> Result<myoci::mirror::MirrorLock, myoci::Error> {
    myoci::mirror::MirrorLock::parse(text)
}

fn entry(body: &str) -> String {
    format!("[[entries]]\n{body}")
}

fn valid_entry(extra: &str) -> String {
    format!(
        r#"[[entries]]
id = "one"
source = "quay.io/example/one@sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4"
destination = "quay.io/example/one"
{extra}"#
    )
}

fn valid_inventory(entries: &[&str]) -> String {
    let mut out = String::from("schema_version = 1\npolicy_version = 1\n\n");
    for entry in entries {
        out.push_str(entry);
        out.push_str("\n");
    }
    out
}

#[test]
fn inventory_and_lock() {
    let seed_inventory = myoci::mirror::Inventory::load(&seed_inventory_path()).unwrap();
    assert_eq!(seed_inventory.entries.len(), 2);
    let arm64 = seed_inventory.entries.iter().find(|entry| entry.id == "manylinux-arm64").unwrap();
    let amd64 = seed_inventory.entries.iter().find(|entry| entry.id == "manylinux-amd64").unwrap();
    assert_eq!(arm64.destination.repository_id(), "quay.io/pypa/manylinux_2_28_aarch64");
    assert_eq!(amd64.destination.repository_id(), "quay.io/pypa/manylinux_2_28_x86_64");
    assert_eq!(arm64.aliases[0].as_str(), "pinned");
    assert_eq!(arm64.platforms[0].render(), "linux/arm64");
    assert_eq!(arm64.referrers, myoci::mirror::ReferrersMode::Snapshot);

    let amd64_pin = std::fs::read_to_string(seed_pins_path("manylinux_2_28-amd64")).unwrap();
    let arm64_pin = std::fs::read_to_string(seed_pins_path("manylinux_2_28-arm64")).unwrap();
    assert!(amd64_pin.contains(amd64.source.reference.as_ref().unwrap().as_str()));
    assert!(arm64_pin.contains(arm64.source.reference.as_ref().unwrap().as_str()));

    let seed_lock = myoci::mirror::MirrorLock::load(&seed_lock_path()).unwrap();
    assert_eq!(seed_lock.entries.len(), 2);
    seed_lock.validate_for_inventory(&seed_inventory).unwrap();
    let lock_amd64 = seed_lock.entry("manylinux-amd64").unwrap();
    assert_eq!(
        lock_amd64.root_digest.as_str(),
        "sha256:7e41b1c7fd64dfc69703a1124b43c404382973ee06ec5d764b85c34d2221fded"
    );
    assert_eq!(lock_amd64.root_size, 5544);
    assert_eq!(lock_amd64.root_media_type, "application/vnd.docker.distribution.manifest.v2+json");
    assert_eq!(lock_amd64.resolved_platforms, vec!["linux/amd64".to_string()]);
    assert!(lock_amd64.additional_referrers.is_empty());
    assert!(seed_lock.entry("manylinux-arm64").unwrap().additional_referrers.is_empty());

    let first = seed_inventory.fingerprint();
    assert_eq!(first, seed_inventory.fingerprint());
    let reordered = format!(
        "schema_version = 1\npolicy_version = 1\n\n{}{}",
        std::fs::read_to_string(seed_inventory_path())
            .unwrap()
            .split("[[entries]]")
            .nth(2)
            .map(|rest| format!("[[entries]]{rest}"))
            .unwrap(),
        std::fs::read_to_string(seed_inventory_path())
            .unwrap()
            .split("[[entries]]")
            .nth(1)
            .map(|rest| format!("[[entries]]{rest}"))
            .unwrap(),
    );
    let reordered_inventory = parse_inventory(&reordered).unwrap();
    assert_eq!(reordered_inventory.fingerprint(), first);

    let round_trip = seed_lock.render();
    let reparsed = parse_lock(&round_trip).unwrap();
    assert_eq!(reparsed, seed_lock);
    assert_eq!(reparsed.render(), round_trip);
    assert_eq!(round_trip.matches("[[entries]]").count(), 2);
    let amd64_pos = round_trip.find("id = \"manylinux-amd64\"").unwrap();
    let arm64_pos = round_trip.find("id = \"manylinux-arm64\"").unwrap();
    assert!(amd64_pos < arm64_pos, "lock entries are not in canonical order");

    for (field, value) in [
        ("schema_version", "2"),
        ("policy_version", "2"),
    ] {
        let bad = format!("schema_version = 1\npolicy_version = 1\n{field} = {value}\n");
        let error = parse_inventory(&bad).unwrap_err();
        assert!(format!("{error}").contains(field), "{error}");
        let error = parse_lock(&bad).unwrap_err();
        assert!(format!("{error}").contains(field), "{error}");
    }

    let bad = "schema_version = 1\npolicy_version = 1\nunknown = true\n";
    let error = parse_inventory(bad).unwrap_err();
    assert!(format!("{error}").contains("unknown field"), "{error}");
    let error = parse_lock(bad).unwrap_err();
    assert!(format!("{error}").contains("unknown field"), "{error}");

    let bad = valid_inventory(&[valid_entry("id_extra = 1\n").as_str()]);
    let error = parse_inventory(&bad).unwrap_err();
    assert!(format!("{error}").contains("unknown field"), "{error}");

    let missing_reference = valid_inventory(&[
        "[[entries]]\nid = \"one\"\nsource = \"quay.io/example/one\"\ndestination = \"quay.io/example/one\"\n",
    ]);
    let error = parse_inventory(&missing_reference).unwrap_err();
    assert!(format!("{error}").contains("tag or digest"), "{error}");

    let destination_with_tag = valid_inventory(&[
        "[[entries]]\nid = \"one\"\nsource = \"quay.io/example/one@sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4\"\ndestination = \"quay.io/example/one:pinned\"\n",
    ]);
    let error = parse_inventory(&destination_with_tag).unwrap_err();
    assert!(format!("{error}").contains("destination"), "{error}");

    let duplicate_ids = valid_inventory(&[
        valid_entry("").as_str(),
        entry("id = \"one\"\nsource = \"quay.io/example/two@sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4\"\ndestination = \"quay.io/example/two\"\n").as_str(),
    ]);
    let error = parse_inventory(&duplicate_ids).unwrap_err();
    assert!(format!("{error}").contains("duplicate entry id"), "{error}");

    let shared_alias = valid_inventory(&[
        valid_entry("aliases = [\"pinned\"]\n").as_str(),
        entry("id = \"two\"\nsource = \"quay.io/example/two@sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4\"\ndestination = \"quay.io/example/one\"\naliases = [\"pinned\"]\n").as_str(),
    ]);
    let error = parse_inventory(&shared_alias).unwrap_err();
    assert!(format!("{error}").contains("pinned"), "{error}");

    let same_alias_other_destination = valid_inventory(&[
        valid_entry("aliases = [\"pinned\"]\n").as_str(),
        entry("id = \"two\"\nsource = \"quay.io/example/two@sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4\"\ndestination = \"quay.io/example/two\"\naliases = [\"pinned\"]\n").as_str(),
    ]);
    parse_inventory(&same_alias_other_destination).unwrap();

    let within_entry_duplicate = valid_inventory(&[valid_entry("aliases = [\"pinned\", \"pinned\"]\n").as_str()]);
    let error = parse_inventory(&within_entry_duplicate).unwrap_err();
    assert!(format!("{error}").contains("duplicate alias"), "{error}");

    let duplicate_platforms = valid_inventory(&[valid_entry("platforms = [\"linux/amd64\", \"linux/amd64\"]\n").as_str()]);
    let error = parse_inventory(&duplicate_platforms).unwrap_err();
    assert!(format!("{error}").contains("duplicate platform"), "{error}");

    for platform in ["Linux/amd64", "linux//amd64", "linux/amd64/v8/extras", "linux/", "/amd64"] {
        let inventory = valid_inventory(&[valid_entry(&format!("platforms = [\"{platform}\"]\n")).as_str()]);
        let error = parse_inventory(&inventory).unwrap_err();
        assert!(format!("{error}").contains("platform"), "{platform}: {error}");
    }

    let variant_requirement = myoci::mirror::PlatformRequirement::parse("linux/arm/v8", "test").unwrap();
    assert_eq!(variant_requirement.render(), "linux/arm/v8");
    let arm_variant = stuff_oci::Platform {
        architecture: "arm".to_string(),
        os: "linux".to_string(),
        variant: Some("v8".to_string()),
        os_version: None,
        features: Vec::new(),
        os_features: Vec::new(),
    };
    let arm_no_variant = stuff_oci::Platform { variant: None, ..arm_variant.clone() };
    assert!(variant_requirement.matches(&arm_variant));
    assert!(!variant_requirement.matches(&arm_no_variant));
    let arm64_requirement = myoci::mirror::PlatformRequirement::parse("linux/arm64", "test").unwrap();
    let arm64_with_variant = stuff_oci::Platform {
        architecture: "arm64".to_string(),
        os: "linux".to_string(),
        variant: Some("v8".to_string()),
        os_version: None,
        features: Vec::new(),
        os_features: Vec::new(),
    };
    assert!(arm64_requirement.matches(&arm64_with_variant));

    let bad_referrers = valid_inventory(&[valid_entry("referrers = \"always\"\n").as_str()]);
    let error = parse_inventory(&bad_referrers).unwrap_err();
    assert!(format!("{error}").contains("referrers"), "{error}");

    let docker_shorthand = valid_inventory(&[
        entry("id = \"one\"\nsource = \"alpine:3.20\"\ndestination = \"index.docker.io/library/alpine\"\n").as_str(),
    ]);
    let inventory = parse_inventory(&docker_shorthand).unwrap();
    assert_eq!(inventory.entries[0].source.repository_id(), "docker.io/library/alpine");
    assert_eq!(inventory.entries[0].destination.repository_id(), "docker.io/library/alpine");

    let bad_lock = seed_lock.render().replace(
        "inventory_fingerprint = \"sha256:3adf6e719c6f3ba6dd9d525e223f2063c601957c12d7c7133c0551d410dc95f5\"",
        "inventory_fingerprint = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"",
    );
    let lock = parse_lock(&bad_lock).unwrap();
    let error = lock.validate_for_inventory(&seed_inventory).unwrap_err();
    assert!(format!("{error}").contains("inventory_fingerprint"), "{error}");

    let unsorted_aliases = seed_lock.render().replace("aliases = [\"pinned\"]", "aliases = [\"beta\", \"alpha\"]");
    let error = parse_lock(&unsorted_aliases).unwrap_err();
    assert!(format!("{error}").contains("canonical order"), "{error}");

    let unsorted_entries = {
        let rendered = seed_lock.render();
        let amd64 = rendered.find("id = \"manylinux-amd64\"").unwrap();
        let arm64 = rendered.find("id = \"manylinux-arm64\"").unwrap();
        let first_entry_start = rendered[..amd64].rfind("[[entries]]").unwrap();
        let second_entry_start = rendered[..arm64].rfind("[[entries]]").unwrap();
        let head = &rendered[..first_entry_start];
        let first = &rendered[first_entry_start..second_entry_start];
        let second = &rendered[second_entry_start..];
        format!("{head}{second}{first}")
    };
    let error = parse_lock(&unsorted_entries).unwrap_err();
    assert!(format!("{error}").contains("canonical order"), "{error}");

    let duplicate_referrers = format!(
        "{}\n[[entries.referrer]]\nsubject = \"sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4\"\nmedia_type = \"application/vnd.oci.image.manifest.v1+json\"\ndigest = \"sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25\"\nsize = 1\n\n[[entries.referrer]]\nsubject = \"sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4\"\nmedia_type = \"application/vnd.oci.image.manifest.v1+json\"\ndigest = \"sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25\"\nsize = 2\n",
        seed_lock.render()
    );
    let error = parse_lock(&duplicate_referrers).unwrap_err();
    assert!(format!("{error}").contains("duplicate referrer"), "{error}");

    let mirror = Command::new(env!("CARGO_BIN_EXE_myoci"))
        .args(["mirror", "resolve", "--config", path(&seed_inventory_path()), "--lock", path(&seed_lock_path())])
        .output()
        .unwrap();
    assert_eq!(mirror.status.code(), Some(1));
    let stderr = String::from_utf8(mirror.stderr).unwrap();
    assert!(stderr.contains("not implemented"), "{stderr}");

    let mirror = Command::new(env!("CARGO_BIN_EXE_myoci"))
        .args(["mirror", "sync", "--config", path(&seed_inventory_path()), "--lock", path(&seed_lock_path()), "--state-dir", ".work/tmp/mirror-state"])
        .output()
        .unwrap();
    assert_eq!(mirror.status.code(), Some(2));
    let stderr = String::from_utf8(mirror.stderr).unwrap();
    assert!(stderr.contains("--locked"), "{stderr}");
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}
