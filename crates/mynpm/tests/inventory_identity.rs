#![forbid(unsafe_code)]

use mynpm::inventory::{
    Inventory, LockfileVersion, Manager, NpmInstallOptions, PackageFlags, PackageRecord,
    PlatformConstraints, ProjectRecord, ProjectSnapshot, SourceKind, merge_project,
    package_identities, parse_inventory, serialize_inventory,
};

fn package(name: &str, version: &str) -> PackageRecord {
    PackageRecord {
        key: format!("{name}@{version}"),
        name: name.to_string(),
        version: Some(version.to_string()),
        integrity: Some("sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".to_string()),
        resolved: Some(format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz")),
        source_kind: SourceKind::Registry,
        aliases: vec![],
        flags: PackageFlags {
            dev: false,
            optional: false,
            peer: false,
            peer_optional: false,
            link: false,
        },
        platform: PlatformConstraints { os: None, cpu: None, libc: None },
        targets: vec!["darwin-arm64".to_string()],
    }
}

fn project(snapshot: Option<ProjectSnapshot>) -> ProjectRecord {
    ProjectRecord {
        id: "demo".to_string(),
        manager: Manager::Pnpm,
        package_manager_pin: Some("pnpm@12.15.0".to_string()),
        lockfile_path: "pnpm-lock.yaml".to_string(),
        lockfile_version: LockfileVersion::Pnpm("9.0".to_string()),
        targets: vec!["darwin-arm64".to_string()],
        importers: vec![".".to_string()],
        roots: vec![],
        packages: vec![package("flatdemo", "1.2.3"), package("@scope/native", "1.0.0")],
        snapshot,
    }
}

fn empty_inventory() -> Inventory {
    Inventory { projects: vec![], artifacts: vec![], metadata_only: vec![] }
}

fn merge_one(base: &Inventory, candidate: ProjectRecord) -> Inventory {
    merge_project(base, candidate).unwrap()
}

fn snapshot(lockfile: &str, workspace_file: &str) -> ProjectSnapshot {
    ProjectSnapshot {
        manifest: Some("{\"name\":\"demo\"}".to_string()),
        lockfile: lockfile.to_string(),
        workspace_files: vec![mynpm::inventory::SnapshotFile {
            path: "tools/package.json".to_string(),
            contents: workspace_file.to_string(),
        }],
        npm_install_options: NpmInstallOptions { legacy_peer_deps: None, install_links: None },
    }
}

#[test]
fn identity_tracks_platform_flags_aliases_and_snapshot() {
    let base = merge_one(&empty_inventory(), project(None));
    assert_eq!(base.projects.len(), 1);
    assert_eq!(merge_one(&base, project(None)).projects.len(), 1);

    let mut platform = project(None);
    platform.packages[0].platform.os = Some(vec!["win32".to_string()]);
    assert_eq!(merge_one(&base, platform).projects.len(), 2);

    let mut flags = project(None);
    flags.packages[0].flags.optional = true;
    assert_eq!(merge_one(&base, flags).projects.len(), 2);

    let mut aliases = project(None);
    aliases.packages[0].aliases = vec!["renamed".to_string()];
    assert_eq!(merge_one(&base, aliases).projects.len(), 2);

    assert_eq!(merge_one(&base, project(Some(snapshot("lock-a", "{\"name\":\"a\"}")))).projects.len(), 2);
    let with_snapshot = merge_one(&base, project(Some(snapshot("lock-a", "{\"name\":\"a\"}"))));
    assert_eq!(
        merge_one(&with_snapshot, project(Some(snapshot("lock-a", "{\"name\":\"b\"}")))).projects.len(),
        3
    );
    assert_eq!(
        merge_one(&with_snapshot, project(Some(snapshot("lock-b", "{\"name\":\"a\"}")))).projects.len(),
        3
    );
}

#[test]
fn duplicate_project_imports_do_not_inflate_package_totals() {
    let first = merge_one(&empty_inventory(), project(Some(snapshot("lock-a", "{\"name\":\"a\"}"))));
    let duplicate = merge_one(&first, project(Some(snapshot("lock-b", "{\"name\":\"a\"}"))));
    assert_eq!(duplicate.projects.len(), 2, "a changed snapshot is a new import");

    let identities = package_identities(&duplicate);
    assert_eq!(identities.len(), 2);
    assert_eq!(identities[0].0, "@scope/native");
    assert_eq!(identities[1].0, "flatdemo");

    let text = serialize_inventory(&duplicate).unwrap();
    let reparsed = parse_inventory(&text).unwrap();
    assert_eq!(reparsed.projects.len(), 2);
    assert_eq!(package_identities(&reparsed).len(), 2);
}
