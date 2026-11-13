#[path = "fixture/registry.rs"]
mod registry;

use registry::{config_json, index_json, manifest_json, referrer_descriptor, Fixture};
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

    let missing = dir_guard();
    let mirror = Command::new(env!("CARGO_BIN_EXE_myoci"))
        .args(["mirror", "resolve", "--config", path(&missing.join("missing.toml")), "--lock", path(&missing.join("missing.lock"))])
        .output()
        .unwrap();
    assert_eq!(mirror.status.code(), Some(1));
    let stderr = String::from_utf8(mirror.stderr).unwrap();
    assert!(stderr.contains("inventory"), "{stderr}");

    let mirror = Command::new(env!("CARGO_BIN_EXE_myoci"))
        .args(["mirror", "plan", "--config", path(&seed_inventory_path()), "--lock", path(&seed_lock_path()), "--state-dir", path(&missing.join("state"))])
        .output()
        .unwrap();
    assert_eq!(mirror.status.code(), Some(0), "{}", String::from_utf8(mirror.stderr).unwrap());
    assert!(!missing.join("state").exists(), "plan must not initialize state");

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

fn dir_guard() -> PathBuf {
    let dir = PathBuf::from(ROOT).join("../../.work/tmp/mirror-cli");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const OCI_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const IN_TOTO: &str = "application/vnd.in-toto+json";
const LAYER_BYTES: &[u8] = b"manylinux layer bytes";

fn inventory_path(dir: &Path) -> PathBuf {
    dir.join("mirror.toml")
}

fn lock_path(dir: &Path) -> PathBuf {
    dir.join("mirror.lock")
}

fn write_inventory(dir: &Path, entries: &[String]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = inventory_path(dir);
    let mut text = String::from("schema_version = 1\npolicy_version = 1\n\n");
    for entry in entries {
        text.push_str(entry);
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
    path
}

fn image_bytes(os: &str, architecture: &str, variant: Option<&str>, layer: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let config_bytes = config_json(os, architecture, variant);
    let config_digest = digest(&config_bytes);
    let layer_digest = digest(layer);
    let manifest = manifest_json(
        &config_digest,
        config_bytes.len() as u64,
        OCI_CONFIG,
        &[(layer_digest, layer.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    (config_bytes, layer.to_vec(), manifest)
}

fn digest(bytes: &[u8]) -> String {
    stuff_oci::Digest::from_bytes(bytes).as_str().to_string()
}

fn single_image_fixture(fixture: &Fixture, os: &str, architecture: &str, variant: Option<&str>, layer: &[u8]) -> (String, String, u64) {
    let (config_bytes, _layer_bytes, manifest) = image_bytes(os, architecture, variant, layer);
    let config_digest = fixture.add_config(&config_bytes);
    fixture.add_blob(layer, OCI_LAYER_GZIP);
    let manifest_size = manifest.len() as u64;
    let root = fixture.add_manifest(manifest, OCI_MANIFEST);
    (root, config_digest, manifest_size)
}

fn write_layout(dir: &Path, os: &str, architecture: &str, layer: &[u8], tag: &str) {
    let (config_bytes, layer_bytes, manifest) = image_bytes(os, architecture, None, layer);
    let blobs = dir.join("blobs/sha256");
    std::fs::create_dir_all(&blobs).unwrap();
    std::fs::write(dir.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
    let manifest_digest = digest(&manifest);
    for bytes in [&config_bytes, &layer_bytes, &manifest] {
        std::fs::write(blobs.join(&digest(bytes)["sha256:".len()..]), bytes).unwrap();
    }
    let index = format!(
        r#"{{"schemaVersion":2,"mediaType":"{}","manifests":[{{"mediaType":"{OCI_MANIFEST}","digest":"{manifest_digest}","size":{},"annotations":{{"org.opencontainers.image.ref.name":"{tag}"}}}}]}}"#,
        OCI_INDEX,
        manifest.len()
    );
    std::fs::write(dir.join("index.json"), index).unwrap();
}


fn mirror_entry(id: &str, source: &str, destination: &str, aliases: &str, platforms: &str, referrers: &str) -> String {
    format!("[[entries]]\nid = \"{id}\"\nsource = \"{source}\"\ndestination = \"{destination}\"\naliases = [{aliases}]\nplatforms = [{platforms}]\nreferrers = \"{referrers}\"\n")
}

fn resolve<'a>(inventory: &Path, lock: &Path, extra: &'a [&str]) -> std::process::Output {
    let mut argv = vec![
        "mirror",
        "resolve",
        "--config",
        path(inventory),
        "--lock",
        path(lock),
    ];
    argv.extend_from_slice(extra);
    Command::new(env!("CARGO_BIN_EXE_myoci")).args(&argv).output().unwrap()
}

#[test]
fn resolve_snapshots() {
    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();

    let fixture = Fixture::new("example/app");
    let (root_digest, _config, root_size) = single_image_fixture(&fixture, "linux", "amd64", None, LAYER_BYTES);
    fixture.add_tag("stable", &root_digest);
    let source = format!("{}/example/app:stable", fixture.address);

    let inventory = write_inventory(
        dir,
        &[mirror_entry("app", &source, "quay.io/example/app", "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let lock = lock_path(dir);

    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let first_bytes = std::fs::read_to_string(&lock).unwrap();
    let resolved = myoci::mirror::MirrorLock::parse(&first_bytes).unwrap();
    let locked_entry = resolved.entry("app").unwrap();
    assert_eq!(locked_entry.root_digest.as_str(), root_digest);
    assert_eq!(locked_entry.root_size, root_size);
    assert_eq!(locked_entry.root_media_type, OCI_MANIFEST);
    assert_eq!(locked_entry.resolved_platforms, vec!["linux/amd64".to_string()]);
    assert_eq!(locked_entry.aliases, vec!["pinned".to_string()]);
    assert_eq!(resolved.inventory_fingerprint, myoci::mirror::Inventory::load(&inventory).unwrap().fingerprint());
    let counts = fixture.counts();
    assert_eq!(counts.blob_get, 0, "resolve must not read layer bodies");
    assert_eq!(counts.config_get, 1, "single-manifest roots read the config once");
    assert_eq!(counts.referrers_get, 1);

    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let second_bytes = std::fs::read_to_string(&lock).unwrap();
    assert_eq!(first_bytes, second_bytes, "unchanged upstream must render identical lock bytes");

    // Tag moves to a different manifest; resolve must record the new root.
    let (other_root, _, _) = single_image_fixture(&fixture, "linux", "amd64", None, b"layer v2");
    fixture.add_tag("stable", &other_root);
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let resolved = myoci::mirror::MirrorLock::parse(&std::fs::read_to_string(&lock).unwrap()).unwrap();
    assert_eq!(resolved.entry("app").unwrap().root_digest.as_str(), other_root);

    // A failed resolution must not touch the lock.
    let unsupported = write_inventory(
        &dir.join("windows"),
        &[mirror_entry("app", &source, "quay.io/example/app", "\"pinned\"", "\"windows/amd64\"", "snapshot")],
    );
    let before = std::fs::read(&lock).unwrap();
    let output = resolve(&unsupported, &lock, &[]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("required platform"), "{stderr}");
    assert_eq!(std::fs::read(&lock).unwrap(), before, "failed resolve rewrote the lock");

    // Referrer discovery failure fails resolution; empty discovery is fine.
    let failing = Fixture::new("example/failing");
    let (failing_root, _, _) = single_image_fixture(&failing, "linux", "amd64", None, LAYER_BYTES);
    failing.add_tag("stable", &failing_root);
    failing.set_referrers_status(500);
    let failing_dir = dir.join("failing");
    std::fs::create_dir_all(&failing_dir).unwrap();
    let failing_inventory = write_inventory(
        &failing_dir,
        &[mirror_entry("failing", &format!("{}/example/failing:stable", failing.address), "quay.io/example/failing", "", "\"linux/amd64\"", "snapshot")],
    );
    let failing_lock = lock_path(&failing_dir);
    let output = resolve(&failing_inventory, &failing_lock, &[]);
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("referrer discovery"), "{stderr}");
    assert!(!failing_lock.exists(), "failed resolution wrote a lock");

    // referrers = "none" skips discovery entirely, even when the API is broken.
    let none_dir = dir.join("none");
    std::fs::create_dir_all(&none_dir).unwrap();
    let none_inventory = write_inventory(
        &none_dir,
        &[mirror_entry("none", &format!("{}/example/failing:stable", failing.address), "quay.io/example/none", "", "\"linux/amd64\"", "none")],
    );
    let none_lock = lock_path(&none_dir);
    let referrers_before = failing.counts().referrers_get;
    let output = resolve(&none_inventory, &none_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert_eq!(failing.counts().referrers_get, referrers_before, "referrers=none must not call the API");

    // Empty referrer observation is a valid, recorded-empty result.
    let empty_dir = dir.join("empty");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let empty_fixture = Fixture::new("example/empty");
    let (empty_root, _, _) = single_image_fixture(&empty_fixture, "linux", "amd64", None, LAYER_BYTES);
    empty_fixture.add_tag("stable", &empty_root);
    let empty_inventory = write_inventory(
        &empty_dir,
        &[mirror_entry("empty", &format!("{}/example/empty:stable", empty_fixture.address), "quay.io/example/empty", "", "\"linux/amd64\"", "snapshot")],
    );
    let empty_lock = lock_path(&empty_dir);
    let output = resolve(&empty_inventory, &empty_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let resolved = myoci::mirror::MirrorLock::parse(&std::fs::read_to_string(&empty_lock).unwrap()).unwrap();
    assert!(resolved.entry("empty").unwrap().additional_referrers.is_empty());
    assert_eq!(empty_fixture.counts().referrers_get, 1);

    // Targeted refresh preserves the unselected entry even when its tag moved upstream.
    let dual_dir = dir.join("dual");
    std::fs::create_dir_all(&dual_dir).unwrap();
    let dual = Fixture::new("example/dual");
    let (a_root, _, _) = single_image_fixture(&dual, "linux", "amd64", None, b"layer a");
    let (b_root, _, _) = single_image_fixture(&dual, "linux", "arm64", None, b"layer b");
    dual.add_tag("stable-a", &a_root);
    dual.add_tag("stable-b", &b_root);
    let dual_inventory = write_inventory(
        &dual_dir,
        &[
            mirror_entry("dual-a", &format!("{}/example/dual:stable-a", dual.address), "quay.io/example/dual-a", "", "\"linux/amd64\"", "none"),
            mirror_entry("dual-b", &format!("{}/example/dual:stable-b", dual.address), "quay.io/example/dual-b", "", "\"linux/arm64\"", "none"),
        ],
    );
    let dual_lock = lock_path(&dual_dir);
    let output = resolve(&dual_inventory, &dual_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let (b_moved, _, _) = single_image_fixture(&dual, "linux", "arm64", None, b"layer b v2");
    dual.add_tag("stable-b", &b_moved);
    let output = resolve(&dual_inventory, &dual_lock, &["--entry", "dual-a"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let locked = myoci::mirror::MirrorLock::parse(&std::fs::read_to_string(&dual_lock).unwrap()).unwrap();
    assert_eq!(locked.entry("dual-a").unwrap().root_digest.as_str(), a_root);
    assert_eq!(locked.entry("dual-b").unwrap().root_digest.as_str(), b_root, "unselected entry must be preserved from the lock");
    let output = resolve(&dual_inventory, &dual_lock, &["--entry", "dual-b"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let locked = myoci::mirror::MirrorLock::parse(&std::fs::read_to_string(&dual_lock).unwrap()).unwrap();
    assert_eq!(locked.entry("dual-b").unwrap().root_digest.as_str(), b_moved);
    assert_eq!(locked.entry("dual-a").unwrap().root_digest.as_str(), a_root);

    // --entry requires a valid lock, known entry names, and an unchanged unselected entry.
    let output = resolve(&dual_inventory, &dual_dir.join("missing.lock"), &["--entry", "dual-a"]);
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("existing valid lock"), "{stderr}");
    let output = resolve(&dual_inventory, &dual_lock, &["--entry", "does-not-exist"]);
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("not in the inventory"), "{stderr}");
    let changed_inventory = write_inventory(
        &dual_dir.join("changed"),
        &[
            mirror_entry("dual-a", &format!("{}/example/dual:stable-a", dual.address), "quay.io/example/dual-a", "", "\"linux/amd64\"", "none"),
            mirror_entry("dual-b", &format!("{}/example/dual:stable-b", dual.address), "quay.io/example/different", "", "\"linux/arm64\"", "none"),
        ],
    );
    let output = resolve(&changed_inventory, &dual_lock, &["--entry", "dual-a"]);
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unselected entry"), "{stderr}");

    // Nested indexes, variants, and embedded attestations.
    let graph_dir = dir.join("graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    let graph = Fixture::new("example/graph");
    let (arm64_root, _, arm64_size) = single_image_fixture(&graph, "linux", "arm", Some("v8"), b"arm layer");
    let empty_config = config_json("unknown", "unknown", None);
    let empty_config_digest = graph.add_config(&empty_config);
    let attestation = manifest_json(
        &empty_config_digest,
        empty_config.len() as u64,
        OCI_CONFIG,
        &[],
        Some((&arm64_root, arm64_size, OCI_MANIFEST)),
        Some(IN_TOTO),
    );
    let attestation_size = attestation.len() as u64;
    let attestation_digest = graph.add_manifest(attestation, OCI_MANIFEST);
    let nested = index_json(
        &[(&arm64_root, arm64_size, OCI_MANIFEST, Some(("linux", "arm", Some("v8"))))],
        None,
        None,
    );
    let nested_size = nested.len() as u64;
    let nested_digest = graph.add_manifest(nested, OCI_INDEX);
    let root_index = index_json(
        &[
            (&nested_digest, nested_size, OCI_INDEX, Some(("linux", "arm", Some("v8")))),
            (&attestation_digest, attestation_size, OCI_MANIFEST, Some(("unknown", "unknown", None))),
        ],
        None,
        None,
    );
    let root_index_digest = graph.add_manifest(root_index, OCI_INDEX);
    graph.add_tag("stable", &root_index_digest);
    let graph_inventory = write_inventory(
        &graph_dir,
        &[mirror_entry("graph", &format!("{}/example/graph:stable", graph.address), "quay.io/example/graph", "", "\"linux/arm/v8\"", "snapshot")],
    );
    let graph_lock = lock_path(&graph_dir);
    let output = resolve(&graph_inventory, &graph_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let resolved = myoci::mirror::MirrorLock::parse(&std::fs::read_to_string(&graph_lock).unwrap()).unwrap();
    assert_eq!(resolved.entry("graph").unwrap().root_digest.as_str(), root_index_digest);
    assert_eq!(resolved.entry("graph").unwrap().resolved_platforms, vec!["linux/arm/v8".to_string()]);
    let counts = graph.counts();
    assert_eq!(counts.blob_get, 0, "attestation and image layers must not be read");
    assert_eq!(counts.config_get, 0, "descriptor platforms were present, no config fetch needed");
    let _ = empty_config_digest;

    // A requirement for a different variant fails; the lock is untouched.
    let wrong_dir = dir.join("wrong");
    std::fs::create_dir_all(&wrong_dir).unwrap();
    let wrong_inventory = write_inventory(
        &wrong_dir,
        &[mirror_entry("wrong", &format!("{}/example/graph:stable", graph.address), "quay.io/example/wrong", "", "\"linux/arm/v7\"", "snapshot")],
    );
    let wrong_lock = lock_path(&wrong_dir);
    let output = resolve(&wrong_inventory, &wrong_lock, &[]);
    assert_eq!(output.status.code(), Some(3));
    assert!(!wrong_lock.exists());

    // Referrers are observed for the root and child manifests and discovered recursively.
    let ref_dir = dir.join("refs");
    std::fs::create_dir_all(&ref_dir).unwrap();
    let refs = Fixture::new("example/refs");
    let (refs_root, _, _) = single_image_fixture(&refs, "linux", "amd64", None, LAYER_BYTES);
    refs.add_tag("stable", &refs_root);
    let att_of_root = referrer_descriptor(OCI_MANIFEST, &format!("sha256:{:0>64}", "11"), 100, Some(IN_TOTO));
    refs.add_referrer(&refs_root, att_of_root);
    let att_root_descriptor_digest = format!("sha256:{:0>64}", "11");
    let att_of_att = referrer_descriptor(OCI_MANIFEST, &format!("sha256:{:0>64}", "22"), 100, Some("application/vnd.unknown"));
    refs.add_referrer(&att_root_descriptor_digest, att_of_att);
    let refs_inventory = write_inventory(
        &ref_dir,
        &[mirror_entry("refs", &format!("{}/example/refs:stable", refs.address), "quay.io/example/refs", "", "\"linux/amd64\"", "snapshot")],
    );
    let refs_lock = lock_path(&ref_dir);
    let output = resolve(&refs_inventory, &refs_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let resolved = myoci::mirror::MirrorLock::parse(&std::fs::read_to_string(&refs_lock).unwrap()).unwrap();
    let referrers = &resolved.entry("refs").unwrap().additional_referrers;
    let pairs: Vec<(&str, &str)> = referrers
        .iter()
        .map(|referrer| (referrer.subject.as_str(), referrer.digest.as_str()))
        .collect();
    let att_of_att_digest = format!("sha256:{:0>64}", "22");
    let mut expected = vec![
        (refs_root.as_str(), att_root_descriptor_digest.as_str()),
        (att_root_descriptor_digest.as_str(), att_of_att_digest.as_str()),
    ];
    let mut pairs = pairs;
    pairs.sort_by_key(|(subject, digest)| (subject.to_string(), digest.to_string()));
    expected.sort_by_key(|(subject, digest)| (subject.to_string(), digest.to_string()));
    assert_eq!(
        pairs, expected,
        "referrers must be recorded with their subjects and deduplicated"
    );
}


fn tree_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        if !dir.is_dir() {
            return;
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap()).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            let file_type = entry.file_type().unwrap();
            if file_type.is_symlink() {
                out.push((relative, format!("symlink:{}", std::fs::read_link(&path).unwrap().display()).into_bytes()));
            } else if file_type.is_dir() {
                walk(&path, root, out);
            } else {
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn myoci_args(args: &[String]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_myoci")).args(args).output().unwrap()
}

fn myoci_publish(state: &Path, site: &Path) -> std::process::Output {
    myoci_args(&[
        "publish".to_string(),
        "--state-dir".to_string(),
        path(state).to_string(),
        "--site-dir".to_string(),
        path(site).to_string(),
    ])
}

fn current_generation(site: &Path) -> String {
    let target = std::fs::read_link(site.join("current")).unwrap();
    target.to_str().unwrap().strip_prefix("generations/").unwrap().to_string()
}

fn diff_files(a: &[(String, Vec<u8>)], b: &[(String, Vec<u8>)]) -> Vec<String> {
    let a: std::collections::BTreeMap<&str, &[u8]> = a.iter().map(|(k, v)| (k.as_str(), v.as_slice())).collect();
    let b: std::collections::BTreeMap<&str, &[u8]> = b.iter().map(|(k, v)| (k.as_str(), v.as_slice())).collect();
    a.keys()
        .chain(b.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|key| a.get(*key) != b.get(*key))
        .map(|key| key.to_string())
        .collect()
}

fn plan_output(inventory: &Path, lock: &Path, state: &Path, extra: &[&str]) -> std::process::Output {
    let mut argv = vec![
        "mirror".to_string(),
        "plan".to_string(),
        "--config".to_string(),
        path(inventory).to_string(),
        "--lock".to_string(),
        path(lock).to_string(),
        "--state-dir".to_string(),
        path(state).to_string(),
    ];
    argv.extend(extra.iter().map(|value| value.to_string()));
    myoci_args(&argv)
}

#[test]
fn plan_is_read_only() {
    let layer_a: &[u8] = b"plan layer A";
    let layer_b: &[u8] = b"plan layer B";

    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();
    let fixture = Fixture::new("example/app");
    let (config_a, _layer_a, _manifest_a) = image_bytes("linux", "amd64", None, layer_a);
    let config_a_digest = fixture.add_config(&config_a);
    fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let manifest_a = manifest_json(
        &config_a_digest,
        config_a.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let a_root = fixture.add_manifest(manifest_a, OCI_MANIFEST);
    fixture.add_tag("stable", &a_root);

    let destination = "docker.io/example/app";
    let source = format!("{}/example/app:stable", fixture.address);
    let inventory = write_inventory(
        dir,
        &[mirror_entry("app", &source, destination, "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let lock = lock_path(dir);
    let state = dir.join("state");
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let lock_pristine = std::fs::read(&lock).unwrap();

    // Absent state: plan succeeds, reports everything missing, creates nothing.
    let output = plan_output(&inventory, &lock, &state, &["--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains(r#""state":"absent""#), "{json}");
    assert!(json.contains(r#""action":"create""#), "{json}");
    assert!(json.contains(r#""result":"clean""#), "{json}");
    assert!(!state.exists(), "plan initialized the state directory");

    // Import image A as an unmanaged "pinned" alias into the state.
    let layout = dir.join("layout-a");
    write_layout(&layout, "linux", "amd64", layer_a, "pinned");
    let output = myoci_args(&[
        "import".to_string(),
        "--state-dir".to_string(),
        path(&state).to_string(),
        "--layout".to_string(),
        path(&layout).to_string(),
        "--repository".to_string(),
        destination.to_string(),
        "--tag".to_string(),
        "pinned".to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // Identical unmanaged alias: adopt (becomes managed). Plan is read-only.
    let tree_before = tree_snapshot(&state);
    let output = plan_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("alias pinned: adopt"), "{stdout}");
    assert_eq!(tree_snapshot(&state), tree_before, "successful plan changed the state tree");

    // Owned and current == desired: unchanged (a managed alias that stays put).
    let ownership_a = format!(
        r#"{{"schemaVersion":1,"aliases":{{"{destination}":{{"pinned":{{"digest":"{a_root}","entry":"app"}}}}}}}}"#
    );
    std::fs::write(state.join("mirror.json"), &ownership_a).unwrap();
    let output = plan_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("alias pinned: unchanged"), "{stdout}");

    // Tag A moved to B in the registry and the lock was re-resolved.
    let (config_b, _layer_b, _manifest_b) = image_bytes("linux", "amd64", None, layer_b);
    let config_b_digest = fixture.add_config(&config_b);
    fixture.add_blob(layer_b, OCI_LAYER_GZIP);
    let manifest_b = manifest_json(
        &config_b_digest,
        config_b.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_b), layer_b.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let b_root = fixture.add_manifest(manifest_b, OCI_MANIFEST);
    fixture.add_tag("stable", &b_root);
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    std::fs::remove_file(state.join("mirror.json")).unwrap();

    // Conflict: unmanaged alias points at A, desired is B, no ownership record.
    let tree_before = tree_snapshot(&state);
    let output = plan_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(4));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("conflict-unmanaged"), "{stdout}");
    assert_eq!(tree_snapshot(&state), tree_before, "failing plan changed the state tree");

    // With an ownership record equal to the current value, the alias may move.
    std::fs::write(state.join("mirror.json"), &ownership_a).unwrap();
    let output = plan_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("alias pinned: move"), "{stdout}");

    // With an ownership record different from the current value, the plan conflicts.
    let stale_ownership = format!(
        r#"{{"schemaVersion":1,"aliases":{{"{destination}":{{"pinned":{{"digest":"sha256:{:0>64}","entry":"app"}}}}}}}}"#,
        "33"
    );
    std::fs::write(state.join("mirror.json"), &stale_ownership).unwrap();
    let tree_before = tree_snapshot(&state);
    let output = plan_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(4));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("conflict-stale"), "{stdout}");
    assert_eq!(tree_snapshot(&state), tree_before, "failing plan changed the state tree");

    // A pending transaction marker fails the plan without recovering it.
    std::fs::remove_file(state.join("mirror.json")).unwrap();
    let pending = state.join(".mirror-transaction");
    std::fs::create_dir_all(&pending).unwrap();
    std::fs::write(pending.join("staged.marker"), b"staged").unwrap();
    let tree_before = tree_snapshot(&state);
    let output = plan_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(4));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("mirror sync --locked"), "{stderr}");
    assert_eq!(tree_snapshot(&state), tree_before, "plan recovered the pending transaction");
    std::fs::remove_dir_all(&pending).unwrap();

    // Deleting the lock cache makes the graph unknown; never zero.
    std::fs::write(state.join("mirror.json"), &ownership_a).unwrap();
    let cache_path = PathBuf::from(format!("{}.cache", lock.display()));
    let cache_bytes = std::fs::read(&cache_path).unwrap();
    std::fs::remove_file(&cache_path).unwrap();
    let tree_before = tree_snapshot(&state);
    let output = plan_output(&inventory, &lock, &state, &["--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains(r#""graph":"unknown""#), "{json}");
    assert!(json.contains(r#""transfer_bytes":null"#), "{json}");
    assert_eq!(tree_snapshot(&state), tree_before);

    // A lock whose bytes changed makes the cache stale; the plan degrades to unknown.
    std::fs::write(&cache_path, &cache_bytes).unwrap();
    let mut stale_lock = lock_pristine.clone();
    stale_lock.extend_from_slice(b"\n# comment is valid TOML but changes the bytes\n");
    std::fs::write(&lock, &stale_lock).unwrap();
    let output = plan_output(&inventory, &lock, &state, &["--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains(r#""graph":"unknown""#), "{json}");
    std::fs::write(&lock, &lock_pristine).unwrap();

    // A lock whose semantic content changed fails the fingerprint check.
    let changed_inventory = write_inventory(
        &dir.join("changed"),
        &[mirror_entry("app", &source, "docker.io/example/other", "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let output = plan_output(&changed_inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(2));
}


fn sync_output(inventory: &Path, lock: &Path, state: &Path, extra: &[&str]) -> std::process::Output {
    let mut argv = vec![
        "mirror".to_string(),
        "sync".to_string(),
        "--config".to_string(),
        path(inventory).to_string(),
        "--lock".to_string(),
        path(lock).to_string(),
        "--state-dir".to_string(),
        path(state).to_string(),
        "--locked".to_string(),
    ];
    argv.extend(extra.iter().map(|value| value.to_string()));
    myoci_args(&argv)
}

fn read_blob_digests(state: &Path) -> Vec<String> {
    myoci::state::State::new(state.to_path_buf())
        .read_blobs()
        .unwrap()
        .into_iter()
        .map(|digest| digest.as_str().to_string())
        .collect()
}

#[test]
fn locked_acquisition() {
    let layer_a: &[u8] = b"acquisition layer A";
    let layer_b: &[u8] = b"acquisition layer B";
    let layer_c: &[u8] = b"acquisition layer C";
    let attestation_layer: &[u8] = b"in-toto attestation payload";

    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();
    let fixture = Fixture::new("example/app");

    // Image A: single-platform manifest with an attestation referrer.
    let (config_a, _layer_a_bytes, _manifest_a) = image_bytes("linux", "amd64", None, layer_a);
    let config_a_digest = fixture.add_config(&config_a);
    fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let manifest_a = manifest_json(
        &config_a_digest,
        config_a.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let a_size = manifest_a.len() as u64;
    let a_root = fixture.add_manifest(manifest_a.clone(), OCI_MANIFEST);
    fixture.add_tag("stable", &a_root);
    let att_layer_digest = fixture.add_blob(attestation_layer, IN_TOTO);
    let att_config = config_json("unknown", "unknown", None);
    let att_config_digest = fixture.add_config(&att_config);
    let att = manifest_json(
        &att_config_digest,
        att_config.len() as u64,
        OCI_CONFIG,
        &[(att_layer_digest.clone(), attestation_layer.len() as u64, IN_TOTO.to_string())],
        Some((&a_root, a_size, OCI_MANIFEST)),
        Some(IN_TOTO),
    );
    let att_size = att.len() as u64;
    let att_digest = fixture.add_manifest(att.clone(), OCI_MANIFEST);
    fixture.add_referrer(&a_root, referrer_descriptor(OCI_MANIFEST, &att_digest, att_size, Some(IN_TOTO)));

    // Image B exists upstream but must not be acquired by the locked sync after re-tagging.
    let (config_b, _layer_b_bytes, _manifest_b) = image_bytes("linux", "amd64", None, layer_b);
    let config_b_digest = fixture.add_config(&config_b);
    fixture.add_blob(layer_b, OCI_LAYER_GZIP);
    let manifest_b = manifest_json(
        &config_b_digest,
        config_b.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_b), layer_b.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let b_root = fixture.add_manifest(manifest_b, OCI_MANIFEST);

    // Image C is unreferenced by the lock; it must not be acquired.
    let (config_c, _layer_c_bytes, _manifest_c) = image_bytes("linux", "amd64", None, layer_c);
    let config_c_digest = fixture.add_config(&config_c);
    fixture.add_blob(layer_c, OCI_LAYER_GZIP);
    let manifest_c = manifest_json(
        &config_c_digest,
        config_c.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_c), layer_c.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let c_root = fixture.add_manifest(manifest_c, OCI_MANIFEST);
    fixture.add_tag("unrelated", &c_root);

    let destination = "docker.io/example/app";
    let source = format!("{}/example/app:stable", fixture.address);
    let inventory = write_inventory(
        dir,
        &[mirror_entry("app", &source, destination, "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let lock = lock_path(dir);
    let state = dir.join("state");
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    let before = fixture.counts();
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let after = fixture.counts();
    assert_eq!(after.blob_get - before.blob_get, 2, "root layer and attestation payload are acquired");
    assert_eq!(after.config_get - before.config_get, 2, "image and attestation configs are acquired");
    let blobs = read_blob_digests(&state);
    assert!(blobs.contains(&digest(layer_a)));
    assert!(blobs.contains(&att_layer_digest));
    assert!(blobs.contains(&config_a_digest));
    assert!(blobs.contains(&att_config_digest));
    assert!(!blobs.contains(&digest(layer_b)));
    assert!(!blobs.contains(&digest(layer_c)));

    let output = myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&state).to_string()]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // The second sync transfers zero bodies.
    let before = fixture.counts();
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let after = fixture.counts();
    assert_eq!(after.blob_get, before.blob_get);
    assert_eq!(after.config_get, before.config_get);
    assert_eq!(after.manifest_get, before.manifest_get);

    // The tag moved to B after the lock; the locked sync must still acquire A.
    fixture.add_tag("stable", &b_root);
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let blobs = read_blob_digests(&state);
    assert!(!blobs.contains(&digest(layer_b)), "locked sync acquired the new tag");
    assert!(blobs.contains(&digest(layer_a)));

    // An attachment added after resolve is not acquired.
    let att2 = manifest_json(
        &att_config_digest,
        att_config.len() as u64,
        OCI_CONFIG,
        &[(att_layer_digest.clone(), attestation_layer.len() as u64, IN_TOTO.to_string())],
        Some((&a_root, a_size, OCI_MANIFEST)),
        Some("application/vnd.other+json"),
    );
    let att2_size = att2.len() as u64;
    let att2_digest = fixture.add_manifest(att2.clone(), OCI_MANIFEST);
    fixture.add_referrer(&a_root, referrer_descriptor(OCI_MANIFEST, &att2_digest, att2_size, Some("application/vnd.other+json")));
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(!state.join("manifests/sha256").join(att2_digest.trim_start_matches("sha256:")).is_file(),
        "attachment added after resolve leaked into the acquisition");
    fixture.add_tag("stable", &a_root);
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(state.join("manifests/sha256").join(att2_digest.trim_start_matches("sha256:")).is_file());

    // Embedded attestations and nested indexes retain the root digest.
    let graph_fixture = Fixture::new("example/graph");
    graph_fixture.add_blob(attestation_layer, IN_TOTO);
    graph_fixture.add_config(&att_config);
    let (arm_config, _arm_layer, _arm_manifest) = image_bytes("linux", "arm", Some("v8"), b"arm layer bytes");
    let arm_config_digest = graph_fixture.add_config(&arm_config);
    let arm_layer_digest = graph_fixture.add_blob(b"arm layer bytes", OCI_LAYER_GZIP);
    let arm_manifest = manifest_json(
        &arm_config_digest,
        arm_config.len() as u64,
        OCI_CONFIG,
        &[(arm_layer_digest.clone(), b"arm layer bytes".len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let arm_size = arm_manifest.len() as u64;
    let arm_root = graph_fixture.add_manifest(arm_manifest.clone(), OCI_MANIFEST);
    let attestation = manifest_json(
        &att_config_digest,
        att_config.len() as u64,
        OCI_CONFIG,
        &[(att_layer_digest.clone(), attestation_layer.len() as u64, IN_TOTO.to_string())],
        Some((&arm_root, arm_size, OCI_MANIFEST)),
        Some(IN_TOTO),
    );
    let attestation_size = attestation.len() as u64;
    let attestation_digest = graph_fixture.add_manifest(attestation.clone(), OCI_MANIFEST);
    let root_index = index_json(
        &[
            (&arm_root, arm_size, OCI_MANIFEST, Some(("linux", "arm", Some("v8")))),
            (&attestation_digest, attestation_size, OCI_MANIFEST, Some(("unknown", "unknown", None))),
        ],
        None,
        None,
    );
    let graph_root = graph_fixture.add_manifest(root_index, OCI_INDEX);
    graph_fixture.add_tag("stable", &graph_root);
    let graph_destination = "docker.io/example/graph";
    let graph_inventory = write_inventory(
        &dir.join("graph"),
        &[mirror_entry("graph", &format!("{}/example/graph:stable", graph_fixture.address), graph_destination, "", "\"linux/arm/v8\"", "snapshot")],
    );
    let graph_lock = lock_path(&dir.join("graph"));
    let graph_state = dir.join("graph-state");
    let output = resolve(&graph_inventory, &graph_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&graph_inventory, &graph_lock, &graph_state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(graph_state.join("manifests/sha256").join(graph_root.trim_start_matches("sha256:")).is_file());
    assert!(graph_state.join("manifests/sha256").join(attestation_digest.trim_start_matches("sha256:")).is_file());
    assert!(graph_state.join("manifests/sha256").join(arm_root.trim_start_matches("sha256:")).is_file());

    // Offline replay fails with the missing identity, then succeeds after re-acquiring.
    let missing_blob = digest(layer_a);
    let blob_path = state.join("blobs/sha256").join(missing_blob.trim_start_matches("sha256:"));
    let saved = std::fs::read(&blob_path).unwrap();
    std::fs::remove_file(&blob_path).unwrap();
    let output = sync_output(&inventory, &lock, &state, &["--offline"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(&missing_blob), "{stderr}");
    let output = myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&state).to_string()]);
    assert_ne!(output.status.code(), Some(0), "offline failure left a corrupt state that verifies");
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert_eq!(std::fs::read(&blob_path).unwrap(), saved);
    let output = sync_output(&inventory, &lock, &state, &["--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // Historical attachments remain even after they disappear from the registry.
    fixture.clear_referrers(&a_root);
    let output = sync_output(&inventory, &lock, &state, &["--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(state.join("manifests/sha256").join(att_digest.trim_start_matches("sha256:")).is_file());

    // A missing descendant is repaired even with the root present.
    let manifest_path = state.join("manifests/sha256").join(att_digest.trim_start_matches("sha256:"));
    let saved_manifest = std::fs::read(&manifest_path).unwrap();
    std::fs::remove_file(&manifest_path).unwrap();
    let output = sync_output(&inventory, &lock, &state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert_eq!(std::fs::read(&manifest_path).unwrap(), saved_manifest);

    // The shared layer is fetched once across entries in one sync.
    let dual = Fixture::new("example/dual");
    let (dual_config_a, _bytes, _manifest) = image_bytes("linux", "amd64", None, layer_a);
    let dual_config_a_digest = dual.add_config(&dual_config_a);
    dual.add_blob(layer_a, OCI_LAYER_GZIP);
    let dual_manifest_a = manifest_json(
        &dual_config_a_digest,
        dual_config_a.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let dual_a = dual.add_manifest(dual_manifest_a, OCI_MANIFEST);
    dual.add_tag("a", &dual_a);
    let dual_destination = "docker.io/example/dual-a";
    let dual_inventory = write_inventory(
        &dir.join("dual"),
        &[
            mirror_entry("dual-a", &format!("{}/example/dual:a", dual.address), dual_destination, "", "\"linux/amd64\"", "none"),
            mirror_entry("dual-a-again", &format!("{}/example/dual:a", dual.address), "docker.io/example/dual-b", "", "\"linux/amd64\"", "none"),
        ],
    );
    let dual_lock = lock_path(&dir.join("dual"));
    let output = resolve(&dual_inventory, &dual_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let dual_state = dir.join("dual-state");
    let before = dual.counts();
    let output = sync_output(&dual_inventory, &dual_lock, &dual_state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let after = dual.counts();
    assert_eq!(after.blob_get - before.blob_get, 1, "the shared layer is fetched once");

    // Unsupported content fails.
    let unsupported_fixture = Fixture::new("example/unsupported");
    let (unsupported_config, _layer, _manifest) = image_bytes("linux", "amd64", None, b"unsupported layer");
    let unsupported_config_digest = unsupported_fixture.add_config(&unsupported_config);
    unsupported_fixture.add_blob(b"unsupported layer", OCI_LAYER_GZIP);
    let unsupported_manifest = manifest_json(
        &unsupported_config_digest,
        unsupported_config.len() as u64,
        OCI_CONFIG,
        &[(digest(b"unsupported layer"), b"unsupported layer".len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let unsupported_size = unsupported_manifest.len() as u64;
    let unsupported_root = unsupported_fixture.add_manifest(unsupported_manifest, OCI_MANIFEST);
    unsupported_fixture.add_tag("stable", &unsupported_root);
    unsupported_fixture.add_config(&att_config);
    unsupported_fixture.add_blob(attestation_layer, IN_TOTO);
    let unsupported_att = manifest_json(
        &unsupported_config_digest,
        att_config.len() as u64,
        OCI_CONFIG,
        &[(att_layer_digest.clone(), attestation_layer.len() as u64, IN_TOTO.to_string())],
        Some((&unsupported_root, unsupported_size, OCI_MANIFEST)),
        Some(IN_TOTO),
    );
    let unsupported_att_size = unsupported_att.len() as u64;
    let unsupported_att_digest = unsupported_fixture.add_manifest(unsupported_att, OCI_MANIFEST);
    unsupported_fixture.add_referrer(&unsupported_root, referrer_descriptor(OCI_MANIFEST, &unsupported_att_digest, unsupported_att_size, Some(IN_TOTO)));
    let unsupported = write_inventory(
        &dir.join("unsupported"),
        &[mirror_entry("unsupported", &format!("{}/example/unsupported:stable", unsupported_fixture.address), "docker.io/example/unsupported", "", "\"linux/amd64\"", "snapshot")],
    );
    let unsupported_lock = lock_path(&dir.join("unsupported"));
    let unsupported_state = dir.join("unsupported-state");
    let output = resolve(&unsupported, &unsupported_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let lock_text = std::fs::read_to_string(&unsupported_lock).unwrap();
    let media_type_line = "media_type = \"application/vnd.oci.image.manifest.v1+json\"";
    let mut in_referrer = false;
    let mut replaced = false;
    let mut lines: Vec<String> = lock_text.lines().map(str::to_string).collect();
    for line in &mut lines {
        if line == "[[entries.referrer]]" {
            in_referrer = true;
        } else if line.starts_with("[[entries]]") {
            in_referrer = false;
        } else if in_referrer && line == media_type_line {
            *line = "media_type = \"application/vnd.oci.artifact.manifest.v1+json\"".to_string();
            replaced = true;
        }
    }
    assert!(replaced, "seed lock carried no referrer with the expected media type");
    std::fs::write(&unsupported_lock, lines.join("\n") + "\n").unwrap();
    let output = sync_output(&unsupported, &unsupported_lock, &unsupported_state, &[]);
    assert_ne!(output.status.code(), Some(0));

    // Corrupt content fails before anything is stored.
    let corrupt_fixture = Fixture::new("example/corrupt");
    let (corrupt_config, _corrupt_layer, _corrupt_manifest) = image_bytes("linux", "amd64", None, b"corrupt layer");
    let corrupt_config_digest = corrupt_fixture.add_config(&corrupt_config);
    corrupt_fixture.add_blob(b"corrupt layer", OCI_LAYER_GZIP);
    let corrupt_manifest = manifest_json(
        &corrupt_config_digest,
        corrupt_config.len() as u64,
        OCI_CONFIG,
        &[(digest(b"corrupt layer"), b"corrupt layer".len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let corrupt_root = corrupt_fixture.add_manifest(corrupt_manifest, OCI_MANIFEST);
    corrupt_fixture.add_tag("stable", &corrupt_root);
    let corrupt_inventory = write_inventory(
        &dir.join("corrupt"),
        &[mirror_entry("corrupt", &format!("{}/example/corrupt:stable", corrupt_fixture.address), "docker.io/example/corrupt", "", "\"linux/amd64\"", "snapshot")],
    );
    let corrupt_lock = lock_path(&dir.join("corrupt"));
    let corrupt_state = dir.join("corrupt-state");
    let output = resolve(&corrupt_inventory, &corrupt_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    corrupt_fixture.tamper_manifest(&corrupt_root);
    let output = sync_output(&corrupt_inventory, &corrupt_lock, &corrupt_state, &[]);
    assert_ne!(output.status.code(), Some(0));
    let manifest_count = if corrupt_state.join("manifests/sha256").is_dir() {
        std::fs::read_dir(corrupt_state.join("manifests/sha256")).unwrap().count()
    } else {
        0
    };
    assert_eq!(manifest_count, 0, "corrupt acquisition stored a manifest");
}

fn sync_ok(output: std::process::Output) -> String {
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "stdout: {stdout}\nstderr: {}", String::from_utf8(output.stderr).unwrap());
    stdout
}

fn sync_err(output: std::process::Output) -> (i32, String, String) {
    let code = output.status.code().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    (code, stdout, stderr)
}

fn snapshot_metadata(state: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let repos = state.join("repositories");
    if repos.is_dir() {
        collect_tree(state, &repos, &mut out);
    }
    for extra in ["mirror.json", "blobs.json"] {
        let path = state.join(extra);
        if path.is_file() {
            out.push((extra.to_string(), std::fs::read(&path).unwrap()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn collect_tree(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap()).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        if entry.file_type().unwrap().is_dir() {
            collect_tree(root, &path, out);
        } else {
            out.push((relative, std::fs::read(&path).unwrap()));
        }
    }
}

fn copy_dir_all(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn stage_transaction(state: &Path, new_metadata: &[(String, Vec<u8>)], marker: bool) {
    let transaction = state.join(myoci::state::MIRROR_TRANSACTION_DIR);
    assert!(!transaction.exists(), "transaction already present in {}", state.display());
    let staged = transaction.join(myoci::state::MIRROR_STAGED_DIR);
    for (relative, bytes) in new_metadata {
        let target = staged.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
    if marker {
        std::fs::write(transaction.join(myoci::state::MIRROR_MARKER), b"committed\n").unwrap();
    }
}

#[test]
fn managed_batch_is_atomic() {
    let layer_a: &[u8] = b"managed batch layer A";
    let layer_b: &[u8] = b"managed batch layer B";

    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();

    // State 1: a two-entry mirror sharing one image, alias "pinned" at each destination.
    let fixture = Fixture::new("example/app");
    let (config_a, _layer_a_bytes, _manifest_a) = image_bytes("linux", "amd64", None, layer_a);
    let config_a_digest = fixture.add_config(&config_a);
    fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let manifest_a = manifest_json(
        &config_a_digest,
        config_a.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let a_root = fixture.add_manifest(manifest_a, OCI_MANIFEST);
    fixture.add_tag("stable", &a_root);

    let source = format!("{}/example/app:stable", fixture.address);
    let inventory = write_inventory(
        dir,
        &[
            mirror_entry("app-one", &source, "docker.io/example/one", "\"pinned\"", "\"linux/amd64\"", "snapshot"),
            mirror_entry("app-two", &source, "docker.io/example/two", "\"pinned\"", "\"linux/amd64\"", "snapshot"),
        ],
    );
    let lock = lock_path(dir);
    let state = dir.join("state");
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // Success: both aliases are created atomically, ownership persisted and reported.
    let output = sync_output(&inventory, &lock, &state, &[]);
    let stdout = sync_ok(output);
    assert!(stdout.contains("alias pinned create"), "{stdout}");
    let ownership = myoci::mirror::ownership::read_ownership(&state).unwrap();
    assert_eq!(ownership.len(), 2);
    assert_eq!(ownership[&("docker.io/example/one".to_string(), "pinned".to_string())].digest.as_str(), a_root);

    // Idempotent re-run: all unchanged; ownership bytes are byte-identical.
    let ownership_before = std::fs::read(state.join("mirror.json")).unwrap();
    let output = sync_output(&inventory, &lock, &state, &[]);
    let stdout = sync_ok(output);
    assert!(stdout.contains("alias pinned unchanged"), "{stdout}");
    assert_eq!(std::fs::read(state.join("mirror.json")).unwrap(), ownership_before);

    // An unrelated repository is byte-identical after a sync.
    let unrelated_layout = dir.join("unrelated-layout");
    write_layout(&unrelated_layout, "linux", "amd64", b"unrelated layer", "stable");
    let output = myoci_args(&[
        "import".to_string(),
        "--state-dir".to_string(),
        path(&state).to_string(),
        "--layout".to_string(),
        path(&unrelated_layout).to_string(),
        "--repository".to_string(),
        "docker.io/example/unrelated".to_string(),
        "--tag".to_string(),
        "stable".to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let unrelated_path = state.join("repositories/docker.io/example/unrelated/repository.json");
    let unrelated_before = std::fs::read(&unrelated_path).unwrap();
    let output = sync_output(&inventory, &lock, &state, &["--offline"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert_eq!(std::fs::read(&unrelated_path).unwrap(), unrelated_before);

    // Unmanaged alias with a different digest: the batch aborts and no aliases move.
    let conflict_layout = dir.join("conflict-layout");
    write_layout(&conflict_layout, "linux", "amd64", layer_b, "tmp");
    let output = myoci_args(&[
        "import".to_string(),
        "--state-dir".to_string(),
        path(&state).to_string(),
        "--layout".to_string(),
        path(&conflict_layout).to_string(),
        "--repository".to_string(),
        "docker.io/example/two".to_string(),
        "--tag".to_string(),
        "tmp".to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let conflict_root = digest(&image_bytes("linux", "amd64", None, layer_b).2);
    std::fs::remove_file(state.join("mirror.json")).unwrap();
    let output = myoci_args(&[
        "tag".to_string(),
        "--state-dir".to_string(),
        path(&state).to_string(),
        "--repository".to_string(),
        "docker.io/example/two".to_string(),
        "--tag".to_string(),
        "pinned".to_string(),
        "--digest".to_string(),
        conflict_root,
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let before = snapshot_metadata(&state);
    let output = sync_output(&inventory, &lock, &state, &[]);
    let (code, _stdout, stderr) = sync_err(output);
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("conflict-unmanaged"), "{stderr}");
    assert_eq!(snapshot_metadata(&state), before, "a conflicting batch applied its aliases");

    // Managed alias whose current value equals the last applied may move.
    let move_dir = dir.join("managed-move");
    std::fs::create_dir_all(&move_dir).unwrap();
    let move_fixture = Fixture::new("example/move");
    let (move_config, _m_layer, _m_manifest) = image_bytes("linux", "amd64", None, layer_a);
    let move_config_digest = move_fixture.add_config(&move_config);
    move_fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let move_manifest = manifest_json(
        &move_config_digest,
        move_config.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let move_root_a = move_fixture.add_manifest(move_manifest, OCI_MANIFEST);
    move_fixture.add_tag("stable", &move_root_a);
    let move_destination = "docker.io/example/move";
    let move_inventory = write_inventory(
        &move_dir,
        &[mirror_entry("move", &format!("{}/example/move:stable", move_fixture.address), move_destination, "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let move_lock = lock_path(&move_dir);
    let move_state = dir.join("move-state");
    let output = resolve(&move_inventory, &move_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&move_inventory, &move_lock, &move_state, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // Tag moves to a different manifest; a managed alias with current == last applied moves.
    let (move_config_b, _b_layer, _b_manifest) = image_bytes("linux", "amd64", None, layer_b);
    let move_config_b_digest = move_fixture.add_config(&move_config_b);
    move_fixture.add_blob(layer_b, OCI_LAYER_GZIP);
    let move_manifest_b = manifest_json(
        &move_config_b_digest,
        move_config_b.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_b), layer_b.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let move_root_b = move_fixture.add_manifest(move_manifest_b, OCI_MANIFEST);
    move_fixture.add_tag("stable", &move_root_b);
    let output = resolve(&move_inventory, &move_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&move_inventory, &move_lock, &move_state, &[]);
    let stdout = sync_ok(output);
    assert!(stdout.contains("alias pinned move"), "{stdout}");
    let ownership = myoci::mirror::ownership::read_ownership(&move_state).unwrap();
    assert_eq!(ownership[&(move_destination.to_string(), "pinned".to_string())].digest.as_str(), move_root_b);

    // A managed alias whose current value differs from the last applied is a stale conflict.
    let output = myoci_args(&[
        "tag".to_string(),
        "--state-dir".to_string(),
        path(&move_state).to_string(),
        "--repository".to_string(),
        move_destination.to_string(),
        "--tag".to_string(),
        "pinned".to_string(),
        "--digest".to_string(),
        move_root_a.clone(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let stale_before = snapshot_metadata(&move_state);
    let output = sync_output(&move_inventory, &move_lock, &move_state, &[]);
    let (code, _stdout, stderr) = sync_err(output);
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("conflict-stale"), "{stderr}");
    assert_eq!(snapshot_metadata(&move_state), stale_before, "stale conflict applied aliases");
    std::fs::remove_file(move_state.join("mirror.json")).unwrap();

    // Unmanaged identical alias is adopted (becomes managed) without any side effects.
    let adopt_dir = dir.join("adopt");
    std::fs::create_dir_all(&adopt_dir).unwrap();
    let adopt_fixture = Fixture::new("example/adopt");
    let (adopt_config, _a_layer, _a_manifest) = image_bytes("linux", "amd64", None, layer_a);
    let adopt_config_digest = adopt_fixture.add_config(&adopt_config);
    adopt_fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let adopt_manifest = manifest_json(
        &adopt_config_digest,
        adopt_config.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let adopt_root = adopt_fixture.add_manifest(adopt_manifest, OCI_MANIFEST);
    adopt_fixture.add_tag("stable", &adopt_root);
    let adopt_destination = "docker.io/example/adopt";
    let adopt_inventory = write_inventory(
        &adopt_dir,
        &[mirror_entry("adopt", &format!("{}/example/adopt:stable", adopt_fixture.address), adopt_destination, "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let adopt_lock = lock_path(&adopt_dir);
    let adopt_state = dir.join("adopt-state");
    let layout_adopt = dir.join("adopt-layout");
    write_layout(&layout_adopt, "linux", "amd64", layer_a, "pinned");
    let output = myoci_args(&[
        "import".to_string(),
        "--state-dir".to_string(),
        path(&adopt_state).to_string(),
        "--layout".to_string(),
        path(&layout_adopt).to_string(),
        "--repository".to_string(),
        adopt_destination.to_string(),
        "--tag".to_string(),
        "pinned".to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = resolve(&adopt_inventory, &adopt_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&adopt_inventory, &adopt_lock, &adopt_state, &[]);
    let stdout = sync_ok(output);
    assert!(stdout.contains("alias pinned adopt"), "{stdout}");
    let ownership = myoci::mirror::ownership::read_ownership(&adopt_state).unwrap();
    assert_eq!(ownership.len(), 1, "identical unmanaged alias did not become managed");

    // Inventory removal: the alias tag stays, ownership is relinquished, objects remain.
    let removal_dir = dir.join("removal");
    std::fs::create_dir_all(&removal_dir).unwrap();
    let removal_state = dir.join("removal-state");
    let removal_inventory_v1 = write_inventory(
        &removal_dir,
        &[mirror_entry("removal", &source, "docker.io/example/removal", "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let removal_lock = lock_path(&removal_dir);
    let output = resolve(&removal_inventory_v1, &removal_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&removal_inventory_v1, &removal_lock, &removal_state, &[]);
    sync_ok(output);
    let removal_ownership = myoci::mirror::ownership::read_ownership(&removal_state).unwrap();
    assert_eq!(removal_ownership.len(), 1);
    let removal_inventory_v2 = write_inventory(
        &removal_dir.join("v2"),
        &[mirror_entry("removal", &source, "docker.io/example/removal", "", "\"linux/amd64\"", "snapshot")],
    );
    let output = resolve(&removal_inventory_v2, &removal_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let tag_before = std::fs::read_to_string(removal_state.join("repositories/docker.io/example/removal/repository.json")).unwrap();
    let output = sync_output(&removal_inventory_v2, &removal_lock, &removal_state, &[]);
    sync_ok(output);
    let tag_after = std::fs::read_to_string(removal_state.join("repositories/docker.io/example/removal/repository.json")).unwrap();
    assert_eq!(tag_before, tag_after, "relinquishing ownership removed the alias");
    let removal_ownership = myoci::mirror::ownership::read_ownership(&removal_state).unwrap();
    assert!(removal_ownership.is_empty(), "removed alias ownership was not relinquished");
    // Removing a whole inventory entry relinquishes its ownership and keeps its content.
    let removal_inventory_v3 = write_inventory(
        &removal_dir.join("v3"),
        &[],
    );
    let output = resolve(&removal_inventory_v3, &removal_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&removal_inventory_v3, &removal_lock, &removal_state, &[]);
    sync_ok(output);
    let kept = std::fs::read_to_string(removal_state.join("repositories/docker.io/example/removal/repository.json")).unwrap();
    assert!(kept.contains("pinned"), "entry removal deleted the alias");
    let ownership = myoci::mirror::ownership::read_ownership(&removal_state).unwrap();
    assert!(ownership.is_empty(), "entry removal kept ownership records");

    // Interruption at every durable transaction boundary: each reader and
    // writer must complete to the full old or new snapshot, never a mix.
    let boundary_dir = dir.join("boundary");
    std::fs::create_dir_all(&boundary_dir).unwrap();
    let boundary_fixture = Fixture::new("example/boundary");
    let (boundary_config_a, _boundary_layer_a, _boundary_manifest_a) = image_bytes("linux", "amd64", None, layer_a);
    let boundary_config_a_digest = boundary_fixture.add_config(&boundary_config_a);
    boundary_fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let boundary_manifest_a = manifest_json(
        &boundary_config_a_digest,
        boundary_config_a.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let boundary_root_a = boundary_fixture.add_manifest(boundary_manifest_a, OCI_MANIFEST);
    boundary_fixture.add_tag("stable", &boundary_root_a);
    let boundary_destination = "docker.io/example/boundary";
    let boundary_source = format!("{}/example/boundary:stable", boundary_fixture.address);
    let boundary_inventory = write_inventory(
        &boundary_dir,
        &[mirror_entry("boundary", &boundary_source, boundary_destination, "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let boundary_lock = lock_path(&boundary_dir);
    let output = resolve(&boundary_inventory, &boundary_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let boundary_state = dir.join("boundary-state");
    let output = sync_output(&boundary_inventory, &boundary_lock, &boundary_state, &[]);
    sync_ok(output);
    let old_metadata = snapshot_metadata(&boundary_state);

    // The interrupted sync is the alias-move sync after the tag moves to B.
    let (boundary_config_b, _boundary_layer_b, _boundary_manifest_b) = image_bytes("linux", "amd64", None, layer_b);
    let boundary_config_b_digest = boundary_fixture.add_config(&boundary_config_b);
    boundary_fixture.add_blob(layer_b, OCI_LAYER_GZIP);
    let boundary_manifest_b = manifest_json(
        &boundary_config_b_digest,
        boundary_config_b.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_b), layer_b.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let boundary_root_b = boundary_fixture.add_manifest(boundary_manifest_b, OCI_MANIFEST);
    boundary_fixture.add_tag("stable", &boundary_root_b);
    let output = resolve(&boundary_inventory, &boundary_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let boundary_new_state = dir.join("boundary-new");
    copy_dir_all(&boundary_state, &boundary_new_state);
    let output = sync_output(&boundary_inventory, &boundary_lock, &boundary_new_state, &[]);
    sync_ok(output);
    let new_metadata = snapshot_metadata(&boundary_new_state);
    assert_ne!(old_metadata, new_metadata, "the alias-move fixture did not change the metadata");
    // The interrupted state is the fully-acquired pre-commit state: old metadata
    // over new content, staged new metadata pending in .mirror-transaction.
    let interrupted_template = dir.join("interrupted-template");
    copy_dir_all(&boundary_new_state, &interrupted_template);
    for (relative, bytes) in &old_metadata {
        let target = interrupted_template.join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, bytes).unwrap();
    }
    let interrupted_before = snapshot_metadata(&interrupted_template);
    assert_eq!(interrupted_before, old_metadata, "interrupted base state disagrees with the old snapshot");

    for boundary in ["no-marker", "marker-no-apply", "marker-partial-apply"] {
        let expected = if boundary == "no-marker" { &old_metadata } else { &new_metadata };
        let alias_digest = if boundary == "no-marker" { &boundary_root_a } else { &boundary_root_b };

        // Plan refuses to proceed while a transaction is pending; it writes nothing.
        let plan_state = dir.join(format!("boundary-{boundary}-plan"));
        copy_dir_all(&interrupted_template, &plan_state);
        stage_transaction(&plan_state, &new_metadata, boundary != "no-marker");
        let plan_before = snapshot_metadata(&plan_state);
        let output = plan_output(&boundary_inventory, &boundary_lock, &plan_state, &[]);
        let (code, _stdout, stderr) = (output.status.code().unwrap(), String::from_utf8(output.stdout).unwrap(), String::from_utf8(output.stderr).unwrap());
        assert_eq!(code, 4, "{stderr}");
        assert!(stderr.contains("mirror transaction"), "{stderr}");
        assert_eq!(snapshot_metadata(&plan_state), plan_before, "plan mutated the interrupted state");

        for command in ["verify", "inspect", "tag", "publish", "sync"] {
            let case_state = dir.join(format!("boundary-{boundary}-{command}"));
            copy_dir_all(&interrupted_template, &case_state);
            stage_transaction(&case_state, &new_metadata, boundary != "no-marker");
            if boundary == "marker-partial-apply" {
                // Real crash mid-apply: exactly one staged file reached its target.
                let mirror_json = new_metadata.iter().find(|(name, _)| name == "mirror.json").unwrap();
                std::fs::write(case_state.join("mirror.json"), &mirror_json.1).unwrap();
            }
            let output = match command {
                "verify" => myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&case_state).to_string()]),
                "inspect" => myoci_args(&["inspect".to_string(), "--state-dir".to_string(), path(&case_state).to_string()]),
                "tag" => myoci_args(&[
                    "tag".to_string(),
                    "--state-dir".to_string(),
                    path(&case_state).to_string(),
                    "--repository".to_string(),
                    boundary_destination.to_string(),
                    "--tag".to_string(),
                    "pinned".to_string(),
                    "--digest".to_string(),
                    alias_digest.to_string(),
                ]),
                "publish" => myoci_args(&[
                    "publish".to_string(),
                    "--state-dir".to_string(),
                    path(&case_state).to_string(),
                    "--site-dir".to_string(),
                    path(&dir.join(format!("boundary-{boundary}-publish-site"))).to_string(),
                ]),
                "sync" => sync_output(&boundary_inventory, &boundary_lock, &case_state, &[]),
                _ => unreachable!(),
            };
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.code(), Some(0), "{command} failed on {boundary}: {stderr}");
            assert!(
                !case_state.join(".mirror-transaction").exists(),
                "{command} left an interrupted transaction on {boundary}"
            );
            if command == "sync" {
                // The re-sync deterministically re-applies the pending move (old state)
                // or reports unchanged (recovered new state); either way the bytes are
                // exactly the new snapshot.
                assert_eq!(snapshot_metadata(&case_state), new_metadata, "sync on {boundary} recovered to mixed state");
                if boundary == "no-marker" {
                    assert!(stdout.contains("alias pinned move"), "{stdout}");
                } else {
                    assert!(stdout.contains("alias pinned unchanged"), "{stdout}");
                }
            } else {
                assert_eq!(snapshot_metadata(&case_state), *expected, "{command} on {boundary} recovered to mixed state");
            }
        }
    }

    // Two concurrent syncs of one fresh state converge to exactly the
    // single-sync metadata for the current lock.
    let fresh_state = dir.join("fresh-state");
    let output = sync_output(&boundary_inventory, &boundary_lock, &fresh_state, &[]);
    sync_ok(output);
    let fresh_metadata = snapshot_metadata(&fresh_state);
    let concurrent_state = dir.join("concurrent-state");
    let spawn_sync = |state: &Path| {
        Command::new(env!("CARGO_BIN_EXE_myoci"))
            .args([
                "mirror",
                "sync",
                "--config",
                path(&boundary_inventory),
                "--lock",
                path(&boundary_lock),
                "--state-dir",
                path(state),
                "--locked",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = spawn_sync(&concurrent_state);
    let second = spawn_sync(&concurrent_state);
    let first_output = first.wait_with_output().unwrap();
    let second_output = second.wait_with_output().unwrap();
    for (index, output) in [&first_output, &second_output].iter().enumerate() {
        assert_eq!(output.status.code(), Some(0), "concurrent sync {index}: {}", String::from_utf8(output.stderr.clone()).unwrap());
    }
    assert_eq!(snapshot_metadata(&concurrent_state), fresh_metadata, "concurrent syncs produced mixed metadata");
    let output = myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&concurrent_state).to_string()]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // Bounded contention: a held state lock fails a child sync fast with exit 4,
    // never an unbounded wait.
    let held_state = dir.join("held-state");
    let output = myoci_args(&["init".to_string(), "--state-dir".to_string(), path(&held_state).to_string()]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let holder = myoci::state::State::new(held_state.clone());
    let _guard = holder.lock().unwrap();
    let started = std::time::Instant::now();
    let output = sync_output(&boundary_inventory, &boundary_lock, &held_state, &[]);
    let elapsed = started.elapsed();
    let (code, _stdout, stderr) = sync_err(output);
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("state is busy"), "{stderr}");
    assert!(elapsed < std::time::Duration::from_secs(10), "sync waited {elapsed:?} on a held state lock");

    // A concurrent import and sync of the same fresh state both complete.
    let mixed_state = dir.join("mixed-state");
    let mixed_layout = dir.join("mixed-layout");
    write_layout(&mixed_layout, "linux", "amd64", b"concurrent import layer", "stable");
    let import_child = Command::new(env!("CARGO_BIN_EXE_myoci"))
        .args([
            "import",
            "--state-dir",
            path(&mixed_state),
            "--layout",
            path(&mixed_layout),
            "--repository",
            "docker.io/example/mixedimport",
            "--tag",
            "stable",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let sync_child = spawn_sync(&mixed_state);
    let import_output = import_child.wait_with_output().unwrap();
    let sync_output_mixed = sync_child.wait_with_output().unwrap();
    assert_eq!(import_output.status.code(), Some(0), "{}", String::from_utf8(import_output.stderr).unwrap());
    assert_eq!(sync_output_mixed.status.code(), Some(0), "{}", String::from_utf8(sync_output_mixed.stderr).unwrap());
    let output = myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&mixed_state).to_string()]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = myoci_args(&["inspect".to_string(), "--state-dir".to_string(), path(&mixed_state).to_string()]);
    let inspection = String::from_utf8(output.stdout).unwrap();
    assert!(inspection.contains("docker.io/example/mixedimport") || inspection.contains("stable"), "{inspection}");
    assert!(mixed_state.join("repositories/docker.io/example/boundary/repository.json").is_file());

    // Adopting a generation published from a synced state restores the sidecar
    // ownership; re-syncing reports the alias unchanged, not created.
    let adopt_state = dir.join("adopt-state");
    let output = sync_output(&boundary_inventory, &boundary_lock, &adopt_state, &[]);
    sync_ok(output);
    let adopt_site = dir.join("adopt-site");
    let output = myoci_args(&[
        "publish".to_string(),
        "--state-dir".to_string(),
        path(&adopt_state).to_string(),
        "--site-dir".to_string(),
        path(&adopt_site).to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let adopted_state = dir.join("adopt-restored");
    let output = myoci_args(&[
        "adopt".to_string(),
        "--state-dir".to_string(),
        path(&adopted_state).to_string(),
        "--site-dir".to_string(),
        path(&adopt_site).to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let restored_repository = std::fs::read_to_string(adopted_state.join("repositories/docker.io/example/boundary/repository.json")).unwrap();
    assert!(restored_repository.contains(&boundary_root_b), "adopted state lost the managed alias: {restored_repository}");
    let output = sync_output(&boundary_inventory, &boundary_lock, &adopted_state, &[]);
    let stdout = sync_ok(output);
    assert!(stdout.contains("alias pinned unchanged"), "{stdout}");
    let ownership = myoci::mirror::ownership::read_ownership(&adopted_state).unwrap();
    assert_eq!(
        ownership[&(boundary_destination.to_string(), "pinned".to_string())].digest.as_str(),
        boundary_root_b
    );
}

#[test]
fn provenance_survives_adoption() {
    let layer: &[u8] = b"provenance layer";

    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();

    let fixture = Fixture::new("example/provenance");
    let (config, layer_bytes, _manifest) = image_bytes("linux", "amd64", None, layer);
    let config_digest = fixture.add_config(&config);
    fixture.add_blob(layer, OCI_LAYER_GZIP);
    let manifest = manifest_json(
        &config_digest,
        config.len() as u64,
        OCI_CONFIG,
        &[(digest(&layer_bytes), layer_bytes.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let root = fixture.add_manifest(manifest, OCI_MANIFEST);
    fixture.add_tag("stable", &root);
    let destination = "docker.io/example/provenance";
    let inventory = write_inventory(
        dir,
        &[mirror_entry("provenance", &format!("{}/example/provenance:stable", fixture.address), destination, "\"pinned\"", "\"linux/amd64\"", "snapshot")],
    );
    let lock = lock_path(dir);
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let state = dir.join("state");
    let output = sync_output(&inventory, &lock, &state, &[]);
    sync_ok(output);

    // The generation carries the stable inventory identity, policy, entries and ownership.
    let site = dir.join("site");
    let output = myoci_publish(&state, &site);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let gen_v1 = current_generation(&site);
    let v1_dir = site.join("generations").join(&gen_v1);
    let sidecar = std::fs::read_to_string(v1_dir.join("provenance.json")).unwrap();
    assert!(sidecar.contains("\"fingerprint\":\"sha256:"), "{sidecar}");
    assert!(sidecar.contains("\"policy\":1"), "{sidecar}");
    assert!(sidecar.contains("\"id\":\"provenance\""), "{sidecar}");
    assert!(sidecar.contains("\"pinned\""), "{sidecar}");
    assert!(sidecar.contains(&root), "{sidecar}");

    // An identical replay is a no-op and keeps the generation ID.
    let output = myoci_publish(&state, &site);
    let replay = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(replay.contains("Already published generation"), "{replay}");
    assert_eq!(current_generation(&site), gen_v1);

    // A provenance-only change produces a different generation ID.
    let provenance = myoci::mirror::provenance::read_provenance(&state).unwrap().expect("sync records provenance");
    let mut changed = provenance.clone();
    let mut fingerprint = changed.fingerprint.clone();
    let last = fingerprint.pop().unwrap();
    fingerprint.push(if last == '0' { '1' } else { '0' });
    changed.fingerprint = fingerprint;
    myoci::mirror::provenance::write_provenance(&state, &changed).unwrap();
    let output = myoci_publish(&state, &site);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let gen_provenance_only = current_generation(&site);
    assert_ne!(gen_provenance_only, gen_v1, "a provenance-only change did not change the generation ID");
    let v1_files = tree_snapshot(&v1_dir);
    let v2_dir = site.join("generations").join(&gen_provenance_only);
    let v2_files = tree_snapshot(&v2_dir);
    let changed_files = diff_files(&v1_files, &v2_files);
    assert_eq!(
        changed_files,
        vec!["catalog.json", "index.html", "llms.txt", "provenance.json"],
        "a provenance-only change touched other files: {changed_files:?}",
    );
    // Both generations are retained for rollback.
    assert!(v1_dir.is_dir(), "the earlier generation was pruned");

    // Rollback to the earlier generation: every referenced object is reachable.
    std::fs::remove_file(site.join("current")).unwrap();
    std::os::unix::fs::symlink(format!("generations/{gen_v1}"), site.join("current")).unwrap();
    let rolled_back = dir.join("rolled-back");
    let output = myoci_args(&[
        "adopt".to_string(),
        "--state-dir".to_string(),
        path(&rolled_back).to_string(),
        "--site-dir".to_string(),
        path(&site).to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let restored_mirror = std::fs::read_to_string(rolled_back.join("mirror.json")).unwrap();
    assert!(restored_mirror.contains("\"entry\":\"provenance\""), "{restored_mirror}");
    assert!(restored_mirror.contains(&root), "{restored_mirror}");
    assert_eq!(
        std::fs::read(rolled_back.join("provenance.json")).unwrap(),
        std::fs::read(v1_dir.join("provenance.json")).unwrap(),
        "adoption changed the sidecar"
    );

    // Adopt followed by republish preserves the generation ID.
    let republished_site = dir.join("republished-site");
    let output = myoci_publish(&rolled_back, &republished_site);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert_eq!(current_generation(&republished_site), gen_v1, "adopt then republish did not preserve the generation");

    // A generation without a sidecar (built before the mirror) remains adoptable.
    let legacy_state = dir.join("legacy-state");
    copy_dir_all(&state, &legacy_state);
    std::fs::remove_file(legacy_state.join("provenance.json")).unwrap();
    let legacy_site = dir.join("legacy-site");
    let output = myoci_publish(&legacy_state, &legacy_site);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let gen_legacy = current_generation(&legacy_site);
    assert!(
        !legacy_site.join("generations").join(&gen_legacy).join("provenance.json").exists(),
        "the legacy generation carries a sidecar",
    );
    let legacy_adopted = dir.join("legacy-adopted");
    let output = myoci_args(&[
        "adopt".to_string(),
        "--state-dir".to_string(),
        path(&legacy_adopted).to_string(),
        "--site-dir".to_string(),
        path(&legacy_site).to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(!legacy_adopted.join("mirror.json").exists(), "a legacy adopt restored ownership from nowhere");
    assert!(!legacy_adopted.join("provenance.json").exists(), "a legacy adopt restored a sidecar from nowhere");
    let output = sync_output(&inventory, &lock, &legacy_adopted, &[]);
    let stdout = sync_ok(output);
    assert!(stdout.contains("alias pinned adopt"), "{stdout}");
    let ownership = myoci::mirror::ownership::read_ownership(&legacy_adopted).unwrap();
    assert_eq!(ownership.len(), 1, "the legacy sync did not re-create ownership");
}

#[test]
fn provenance_survives_targeted_refresh() {
    let layer_a: &[u8] = b"provenance targeted A";
    let layer_b: &[u8] = b"provenance targeted B";

    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();

    let fixture = Fixture::new("example/carry");
    let (config_a, _bytes_a, _manifest_a) = image_bytes("linux", "amd64", None, layer_a);
    let config_a_digest = fixture.add_config(&config_a);
    fixture.add_blob(layer_a, OCI_LAYER_GZIP);
    let manifest_a = manifest_json(
        &config_a_digest,
        config_a.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_a), layer_a.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let root_a = fixture.add_manifest(manifest_a, OCI_MANIFEST);
    fixture.add_tag("stable", &root_a);
    fixture.add_tag("canary", &root_a);

    let carry_dir = dir.join("carry");
    std::fs::create_dir_all(&carry_dir).unwrap();
    let entry_one = mirror_entry("carry-one", &format!("{}/example/carry:stable", fixture.address), "docker.io/example/carry-one", "\"pinned\"", "\"linux/amd64\"", "snapshot");
    let entry_two = mirror_entry("carry-two", &format!("{}/example/carry:canary", fixture.address), "docker.io/example/carry-two", "\"pinned\"", "\"linux/amd64\"", "snapshot");
    let inventory = write_inventory(&carry_dir, &[entry_one, entry_two]);
    let lock = lock_path(&carry_dir);
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let state = dir.join("carry-state");
    let output = sync_output(&inventory, &lock, &state, &[]);
    sync_ok(output);
    let before = myoci::mirror::provenance::read_provenance(&state).unwrap().expect("sync records provenance");

    // Only the canary entry moves; targeted refresh must leave carry-one untouched.
    let (config_b, _bytes_b, _manifest_b) = image_bytes("linux", "amd64", None, layer_b);
    let config_b_digest = fixture.add_config(&config_b);
    fixture.add_blob(layer_b, OCI_LAYER_GZIP);
    let manifest_b = manifest_json(
        &config_b_digest,
        config_b.len() as u64,
        OCI_CONFIG,
        &[(digest(layer_b), layer_b.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let root_b = fixture.add_manifest(manifest_b, OCI_MANIFEST);
    fixture.add_tag("canary", &root_b);
    let output = resolve(&inventory, &lock, &["--entry", "carry-two"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = sync_output(&inventory, &lock, &state, &[]);
    sync_ok(output);
    let after = myoci::mirror::provenance::read_provenance(&state).unwrap().expect("sync records provenance");

    let entry_before = |provenance: &myoci::mirror::provenance::Provenance, id: &str| provenance.entries.iter().find(|entry| entry.id == id).unwrap().clone();
    let ownership_before = |provenance: &myoci::mirror::provenance::Provenance, destination: &str| {
        provenance
            .ownership
            .iter()
            .find(|record| record.destination == destination)
            .unwrap()
            .clone()
    };
    assert_eq!(entry_before(&before, "carry-one"), entry_before(&after, "carry-one"), "targeted refresh changed carry-one provenance");
    assert_eq!(entry_before(&before, "carry-two").root, root_a);
    assert_eq!(entry_before(&after, "carry-two").root, root_b);
    assert_eq!(
        ownership_before(&before, "docker.io/example/carry-one"),
        ownership_before(&after, "docker.io/example/carry-one"),
        "targeted refresh changed carry-one ownership",
    );
    assert_eq!(ownership_before(&after, "docker.io/example/carry-two").digest, root_b);

    // A replay after the targeted refresh preserves both records byte-for-byte.
    let replayed = after.render();
    let output = sync_output(&inventory, &lock, &state, &[]);
    sync_ok(output);
    let replay = myoci::mirror::provenance::read_provenance(&state).unwrap().unwrap();
    assert_eq!(replay.render(), replayed, "identical replay rewrote the sidecar");
}

#[test]
fn operational_contract() {
    // Fixture: one amd64 image, tags "stable" and "canary" both pointing at A.
    let workspace = tempfile::tempdir().unwrap();
    let dir = workspace.path();
    let layer_a: &[u8] = b"operational contract layer A";
    let layer_b: &[u8] = b"operational contract layer B";

    let fixture = Fixture::new("example/app");
    let (root_a, _config_a, _size_a) = single_image_fixture(&fixture, "linux", "amd64", None, layer_a);
    fixture.add_tag("stable", &root_a);
    fixture.add_tag("canary", &root_a);

    // Add: resolve the two-entry inventory; both entries land in the lock.
    let inventory = write_inventory(
        dir,
        &[
            mirror_entry("contract-one", &format!("{}/example/app:stable", fixture.address), "docker.io/example/contract-one", "\"pinned\"", "\"linux/amd64\"", "none"),
            mirror_entry("contract-two", &format!("{}/example/app:canary", fixture.address), "docker.io/example/contract-two", "\"pinned\"", "\"linux/amd64\"", "none"),
        ],
    );
    let lock = lock_path(dir);
    let output = resolve(&inventory, &lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let lock_text = std::fs::read_to_string(&lock).unwrap();
    assert!(lock_text.contains("contract-one"));
    assert!(lock_text.contains("contract-two"));

    // Selected refresh: only contract-two moves; contract-one is preserved byte for byte.
    let (root_b, _config_b, _size_b) = single_image_fixture(&fixture, "linux", "amd64", None, layer_b);
    fixture.add_tag("canary", &root_b);
    let output = resolve(&inventory, &lock, &["--entry", "contract-two"]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let lock_text = std::fs::read_to_string(&lock).unwrap();
    let one = lock_text.split("contract-two").next().unwrap();
    assert!(one.contains(&root_a), "targeted refresh moved contract-one: {one}");
    // The targeted refresh preserves contract-one's cached graph for plan estimates.
    let output = plan_output(&inventory, &lock, &dir.join("contract-plan-state"), &[]);
    let plan_text = String::from_utf8(output.stdout).unwrap();
    assert!(plan_text.contains("contract-one (destination docker.io/example/contract-one):"), "{plan_text}");
    let one_section = plan_text.split("entry contract-two").next().unwrap();
    assert!(one_section.contains("3 missing of 3"), "{one_section}");

    // Review: plan is read-only, reports the batch, and --json parses.
    let state = dir.join("state");
    let output = plan_output(&inventory, &lock, &state, &["--json"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(stdout.contains("\"entries\":["), "{stdout}");
    let before_absent = snapshot_metadata(&state);
    let output = plan_output(&inventory, &lock, &state, &[]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("plan: clean"), "{stdout}");
    assert_eq!(snapshot_metadata(&state), before_absent, "plan mutated state");

    // Locked sync: aliases applied atomically; ownership recorded.
    let output = sync_output(&inventory, &lock, &state, &[]);
    sync_ok(output);
    let ownership = myoci::mirror::ownership::read_ownership(&state).unwrap();
    assert_eq!(ownership.len(), 2);

    // Offline replay: zero upstream requests after the first sync.
    let counts_before = fixture.counts();
    let output = sync_output(&inventory, &lock, &state, &["--offline"]);
    sync_ok(output);
    let counts_after = fixture.counts();
    assert_eq!(counts_after.manifest_get, counts_before.manifest_get, "offline replay fetched a manifest");
    assert_eq!(counts_after.blob_get, counts_before.blob_get, "offline replay fetched a blob");
    assert_eq!(counts_after.blob_head, counts_before.blob_head, "offline replay HEADed a blob");
    assert_eq!(counts_after.config_get, counts_before.config_get, "offline replay fetched a config");
    assert_eq!(counts_after.referrers_get, counts_before.referrers_get, "offline replay enumerated referrers");

    // Publish: a generation with the deterministic sidecar is produced and adoptable.
    let site = dir.join("site");
    let output = myoci_publish(&state, &site);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let generation = current_generation(&site);
    let sidecar = site.join("generations").join(&generation).join("provenance.json");
    assert!(sidecar.is_file(), "the published generation carries no provenance sidecar");
    let rolled_back = dir.join("rolled-back");
    let output = myoci_args(&[
        "adopt".to_string(),
        "--state-dir".to_string(),
        path(&rolled_back).to_string(),
        "--site-dir".to_string(),
        path(&site).to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&rolled_back).to_string()]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());

    // Missing digest: offline sync with no local content fails and names what is missing.
    let fresh_state = dir.join("fresh-state");
    let counts_before = fixture.counts();
    let output = sync_output(&inventory, &lock, &fresh_state, &["--offline"]);
    let (code, _stdout, stderr) = sync_err(output);
    assert_ne!(code, 0, "offline sync with empty state succeeded");
    assert!(stderr.contains("missing"), "{stderr}");
    assert_eq!(fixture.counts().manifest_get, counts_before.manifest_get, "offline replay hit the network");
    let absent_state = dir.join("never-created-state");
    let output = plan_output(&inventory, &lock, &absent_state, &[]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    assert!(stdout.contains("every object is missing"), "{stdout}");
    assert!(!absent_state.exists(), "plan initialized the state directory");

    // Conflict: an unmanaged alias with a different digest aborts the batch with nothing applied.
    let layer_c: &[u8] = b"operational contract layer C";
    let conflict_dir = dir.join("conflict");
    std::fs::create_dir_all(&conflict_dir).unwrap();
    let conflict_layout = dir.join("conflict-layout");
    write_layout(&conflict_layout, "linux", "amd64", layer_c, "pinned");
    let output = myoci_args(&[
        "import".to_string(),
        "--state-dir".to_string(),
        path(&state).to_string(),
        "--layout".to_string(),
        path(&conflict_layout).to_string(),
        "--repository".to_string(),
        "docker.io/example/contract-three".to_string(),
        "--tag".to_string(),
        "pinned".to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let conflict_inventory = write_inventory(
        &conflict_dir,
        &[
            mirror_entry("contract-one", &format!("{}/example/app:stable", fixture.address), "docker.io/example/contract-one", "\"pinned\"", "\"linux/amd64\"", "none"),
            mirror_entry("contract-two", &format!("{}/example/app:canary", fixture.address), "docker.io/example/contract-two", "\"pinned\"", "\"linux/amd64\"", "none"),
            mirror_entry("contract-three", &format!("{}/example/app:canary", fixture.address), "docker.io/example/contract-three", "\"pinned\"", "\"linux/amd64\"", "none"),
        ],
    );
    let conflict_lock = lock_path(&conflict_dir);
    let output = resolve(&conflict_inventory, &conflict_lock, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let before = snapshot_metadata(&state);
    let output = sync_output(&conflict_inventory, &conflict_lock, &state, &[]);
    let (code, _stdout, stderr) = sync_err(output);
    assert_eq!(code, 4, "{stderr}");
    assert!(stderr.contains("conflict-unmanaged"), "{stderr}");
    assert_eq!(snapshot_metadata(&state), before, "a conflicting batch moved aliases");
    let output = plan_output(&conflict_inventory, &conflict_lock, &state, &[]);
    let stderr_plan = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(4), "{stderr_plan}");
    assert!(stderr_plan.contains("alias conflict"), "{stderr_plan}");
    let stdout_plan = String::from_utf8(output.stdout).unwrap();
    assert!(stdout_plan.contains("conflict-unmanaged"), "{stdout_plan}");

    // Documentation example: import a layout, inspect, verify — all succeed.
    let readme_state = dir.join("readme-state");
    let readme_layout = dir.join("readme-layout");
    write_layout(&readme_layout, "linux", "amd64", b"readme doc layer", "stable");
    let output = myoci_args(&[
        "import".to_string(),
        "--state-dir".to_string(),
        path(&readme_state).to_string(),
        "--layout".to_string(),
        path(&readme_layout).to_string(),
        "--repository".to_string(),
        "example/readme".to_string(),
        "--tag".to_string(),
        "stable".to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
    let output = myoci_args(&["inspect".to_string(), "--state-dir".to_string(), path(&readme_state).to_string()]);
    assert_eq!(output.status.code(), Some(0));
    let inspection = String::from_utf8(output.stdout).unwrap();
    assert!(inspection.contains("stable"), "{inspection}");
    let output = myoci_args(&["verify".to_string(), "--state-dir".to_string(), path(&readme_state).to_string()]);
    assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8(output.stderr).unwrap());
}
