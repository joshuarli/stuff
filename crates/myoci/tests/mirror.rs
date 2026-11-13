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
    assert_eq!(mirror.status.code(), Some(1));
    let stderr = String::from_utf8(mirror.stderr).unwrap();
    assert!(stderr.contains("not implemented"), "{stderr}");
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

fn single_image_fixture(fixture: &Fixture, os: &str, architecture: &str, variant: Option<&str>, layer: &[u8]) -> (String, String, u64) {
    let config_bytes = config_json(os, architecture, variant);
    let config_digest = fixture.add_config(&config_bytes);
    let layer_digest = fixture.add_blob(layer, OCI_LAYER_GZIP);
    let manifest = manifest_json(
        &config_digest,
        config_bytes.len() as u64,
        OCI_CONFIG,
        &[(layer_digest, layer.len() as u64, OCI_LAYER_GZIP.to_string())],
        None,
        None,
    );
    let manifest_size = manifest.len() as u64;
    let root = fixture.add_manifest(manifest, OCI_MANIFEST);
    (root, config_digest, manifest_size)
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

