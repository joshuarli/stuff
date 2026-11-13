use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use mymirror::{Acquire, Error, build, fetch_verified, manifest::Manifest};

struct FakeAcquire {
    responses: HashMap<String, Vec<u8>>,
}

impl Acquire for FakeAcquire {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.responses
            .get(url)
            .cloned()
            .ok_or_else(|| Error::Transport(format!("no fake response for {url}")))
    }
}

fn tar_gz(entries: &[(&str, &[u8], u32, u8)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data, mode, typeflag) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        match *typeflag {
            b'2' => {
                header.set_entry_type(tar::EntryType::symlink());
                header.set_size(0);
                header.set_link_name_literal(data).unwrap();
                header.set_cksum();
                builder.append_data(&mut header, name, std::io::empty()).unwrap();
            }
            _ => {
                header.set_entry_type(tar::EntryType::file());
                header.set_size(data.len() as u64);
                header.set_cksum();
                builder.append_data(&mut header, name, *data).unwrap();
            }
        }
    }
    let tar_bytes = builder.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

fn tar_xz(entries: &[(&str, &[u8], u32, u8)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data, mode, _typeflag) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        header.set_entry_type(tar::EntryType::file());
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(tar_bytes), &mut out).unwrap();
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    mymirror::sha256_hex(bytes)
}

fn raw_tar(name: &str, data: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    let name_bytes = name.as_bytes();
    header[..name_bytes.len()].copy_from_slice(name_bytes);
    let octal = |value: u64, width: usize| {
        let mut out = vec![0u8; width];
        let digits = format!("{value:o}");
        let start = width - digits.len() - 1;
        out[..start].fill(b'0');
        out[start..start + digits.len()].copy_from_slice(digits.as_bytes());
        out[start + digits.len()] = 0;
        out
    };
    header[100..108].copy_from_slice(&octal(0o644, 8));
    header[108..116].copy_from_slice(&octal(0, 8));
    header[116..124].copy_from_slice(&octal(0, 8));
    header[124..136].copy_from_slice(&octal(data.len() as u64, 12));
    header[136..148].copy_from_slice(&octal(0, 12));
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum: u64 = header.iter().map(|&byte| u64::from(byte)).sum();
    let chk = format!("{checksum:06o}");
    header[148..156].copy_from_slice(format!("{chk}\0 ").as_bytes());
    let mut out = Vec::with_capacity(1024 + data.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out.extend_from_slice(&[0; 1024]);
    out
}

fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn manifest_for(entries: &[(&str, &str, &str)]) -> Manifest {
    let mut text = String::from("schema_version = 1\n");
    for (id, url, sha256) in entries {
        text.push_str(&format!(
            "\n[[artifact]]\nid = \"{id}\"\nversion = \"1.0.0\"\nurl = \"{url}\"\nsha256 = \"{sha256}\"\n"
        ));
    }
    Manifest::parse(&text).unwrap()
}

fn artifact_url(name: &str) -> String {
    format!("https://upstream.example.test/{name}")
}

fn fake_acquire(entries: &[(&str, Vec<u8>)]) -> FakeAcquire {
    FakeAcquire {
        responses: entries
            .iter()
            .map(|(name, bytes)| (artifact_url(name), bytes.clone()))
            .collect(),
    }
}

fn work_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("staging");
    let cache = root.path().join("cache");
    (root, staging, cache)
}

#[test]
fn a_tar_gz_upstream_becomes_a_zip_with_stripped_members() {
    let archive = tar_gz(&[
        ("pkg-1.0.0/bin/app", b"#!/bin/sh\n", 0o755, b'0'),
        ("pkg-1.0.0/bin/tool", b"tool", 0o644, b'0'),
        ("pkg-1.0.0/README.md", b"docs", 0o644, b'0'),
        ("pkg-1.0.0/lib/helper.so", b"lib", 0o644, b'0'),
    ]);
    let url = artifact_url("app.tar.gz");
    let sums = fake_acquire(&[("app.tar.gz", archive.clone())]);
    let manifest = Manifest::parse(&format!(
        "schema_version = 1\n[[artifact]]\nid = \"app\"\nversion = \"1.0.0\"\nurl = \"{url}\"\nsha256 = \"{}\"\nstrip_components = 1\nkeep = [\"bin/**\", \"lib/**\"]\n",
        sha256_hex(&archive)
    ))
    .unwrap();
    let (_root, staging, cache) = work_dirs();
    let built = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    assert_eq!(built.artifacts.len(), 1);
    let artifact = &built.artifacts[0];
    assert_eq!(artifact.files, vec!["bin/app", "bin/tool", "lib/helper.so"]);
    assert_eq!(artifact.file, "app.zip");
    let zip_bytes = std::fs::read(&artifact.path).unwrap();
    assert_eq!(zip_bytes.len() as u64, artifact.bytes);
    assert_eq!(sha256_hex(&zip_bytes), artifact.sha256);
    let sums_text = &built.sums;
    assert_eq!(sums_text, &format!("{}  app.zip\n", artifact.sha256));
    assert!(built.catalog.contains("\"registry\": \"mirror\""));
    assert!(built.catalog.contains(&format!("\"generation\": \"{}\"", built.generation)));
}

#[test]
fn builds_are_deterministic() {
    let archive = tar_gz(&[
        ("root/bin/app", b"bin", 0o755, b'0'),
        ("root/share/data.txt", b"data", 0o644, b'0'),
    ]);
    let manifest = manifest_for(&[("app", &artifact_url("app.tar.gz"), &sha256_hex(&archive))]);
    let sums = fake_acquire(&[("app.tar.gz", archive)]);
    let (_root, staging, cache) = work_dirs();
    let first = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    let (_root2, staging2, cache2) = work_dirs();
    let second = build(&manifest, "https://mirror.example.test", &sums, Some(&cache2), &staging2).unwrap();
    assert_eq!(first.generation, second.generation);
    assert_eq!(first.artifacts[0].sha256, second.artifacts[0].sha256);
    assert_eq!(first.catalog, second.catalog);
    assert_eq!(first.sums, second.sums);
}

#[test]
fn a_tar_xz_upstream_is_supported() {
    let archive = tar_xz(&[("root/file.txt", b"content", 0o644, b'0')]);
    let url = artifact_url("app.tar.xz");
    let sums = fake_acquire(&[("app.tar.xz", archive.clone())]);
    let manifest = Manifest::parse(&format!(
        "schema_version = 1\n[[artifact]]\nid = \"app\"\nversion = \"1.0.0\"\nurl = \"{url}\"\nsha256 = \"{}\"\nstrip_components = 1\n",
        sha256_hex(&archive)
    ))
    .unwrap();
    let (_root, staging, cache) = work_dirs();
    let built = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    assert_eq!(built.artifacts[0].files, vec!["file.txt"]);
}

#[test]
fn a_raw_file_is_stored_under_its_url_name() {
    let url = artifact_url("install.sh");
    let sums = fake_acquire(&[("install.sh", b"#!/bin/sh\necho hi\n".to_vec())]);
    let manifest = manifest_for(&[("tool-install", &url, &sha256_hex(b"#!/bin/sh\necho hi\n"))]);
    let (_root, staging, cache) = work_dirs();
    let built = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    assert_eq!(built.artifacts[0].files, vec!["install.sh"]);
}

#[test]
fn a_sha256_mismatch_is_an_integrity_failure() {
    let archive = tar_gz(&[("root/file.txt", b"content", 0o644, b'0')]);
    let url = artifact_url("app.tar.gz");
    let sums = fake_acquire(&[("app.tar.gz", archive)]);
    let manifest = manifest_for(&[("app", &url, &"0".repeat(64))]);
    let (_root, staging, cache) = work_dirs();
    let error = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap_err();
    assert!(matches!(error, Error::Integrity(_)), "{error}");
}

#[test]
fn the_cache_avoids_refetching_and_revalidates_it() {
    let archive = tar_gz(&[("root/file.txt", b"content", 0o644, b'0')]);
    let url = artifact_url("app.tar.gz");
    let sha256 = sha256_hex(&archive);
    let sums = fake_acquire(&[("app.tar.gz", archive)]);
    let (_root, _staging, cache) = work_dirs();
    let first = fetch_verified(&sums, Some(&cache), &url, &sha256).unwrap();
    assert_eq!(sha256_hex(&first), sha256);
    let failing = FakeAcquire { responses: HashMap::new() };
    let second = fetch_verified(&failing, Some(&cache), &url, &sha256).unwrap();
    assert_eq!(sha256_hex(&second), sha256);
    std::fs::write(cache.join(&sha256), b"corrupted").unwrap();
    let error = fetch_verified(&failing, Some(&cache), &url, &sha256).unwrap_err();
    assert!(matches!(error, Error::Transport(_)), "{error}");
}

#[test]
fn a_keep_list_matching_nothing_is_an_error() {
    let archive = tar_gz(&[("root/file.txt", b"content", 0o644, b'0')]);
    let url = artifact_url("app.tar.gz");
    let manifest = Manifest::parse(&format!(
        "schema_version = 1\n[[artifact]]\nid = \"app\"\nversion = \"1\"\nurl = \"{url}\"\nsha256 = \"{}\"\nkeep = [\"missing/**\"]\n",
        sha256_hex(&archive)
    ))
    .unwrap();
    let sums = fake_acquire(&[("app.tar.gz", archive)]);
    let (_root, staging, cache) = work_dirs();
    let error = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap_err();
    assert!(error.to_string().contains("no files matched the keep list"), "{error}");
}

#[test]
fn unsafe_member_paths_are_rejected() {
    for (name, bad) in [
        ("absolute", "/etc/passwd"),
        ("parent", "pkg/../escape.txt"),
        ("drive", "C:/windows/system32.dll"),
        ("backslash", "pkg\\member.txt"),
    ] {
        let archive = gz(&raw_tar(bad, b"x"));
        let url = artifact_url("bad.tar.gz");
        let sums = fake_acquire(&[("bad.tar.gz", archive.clone())]);
        let manifest = manifest_for(&[("bad", &url, &sha256_hex(&archive))]);
        let (_root, staging, cache) = work_dirs();
        let error = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap_err();
        assert!(error.to_string().contains("is not safe"), "{name}: {error}");
    }
}

#[test]
fn symlinks_escaping_the_root_are_rejected() {
    let archive = tar_gz(&[("pkg-1.0.0/bin/link", b"../../../etc/passwd", 0o777, b'2')]);
    let url = artifact_url("link.tar.gz");
    let sums = fake_acquire(&[("link.tar.gz", archive.clone())]);
    let manifest = manifest_for(&[("link", &url, &sha256_hex(&archive))]);
    let (_root, staging, cache) = work_dirs();
    let error = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap_err();
    assert!(error.to_string().contains("escaping the archive root"), "{error}");
}

#[test]
fn symlinks_inside_the_root_are_kept() {
    let archive = tar_gz(&[
        ("pkg-1.0.0/lib/real.js", b"js", 0o644, b'0'),
        ("pkg-1.0.0/bin/link", b"../lib/real.js", 0o777, b'2'),
    ]);
    let url = artifact_url("ok.tar.gz");
    let sums = fake_acquire(&[("ok.tar.gz", archive.clone())]);
    let manifest = Manifest::parse(&format!(
        "schema_version = 1\n[[artifact]]\nid = \"ok\"\nversion = \"1\"\nurl = \"{url}\"\nsha256 = \"{}\"\nstrip_components = 1\n",
        sha256_hex(&archive)
    ))
    .unwrap();
    let (_root, staging, cache) = work_dirs();
    let built = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    assert_eq!(built.artifacts[0].files, vec!["bin/link", "lib/real.js"]);
}

#[test]
fn duplicate_members_after_stripping_are_an_error() {
    let archive = tar_gz(&[
        ("pkg/a/file.txt", b"one", 0o644, b'0'),
        ("pkg/b/file.txt", b"two", 0o644, b'0'),
    ]);
    let url = artifact_url("dup.tar.gz");
    let sums = fake_acquire(&[("dup.tar.gz", archive.clone())]);
    let manifest = Manifest::parse(&format!(
        "schema_version = 1\n[[artifact]]\nid = \"dup\"\nversion = \"1\"\nurl = \"{url}\"\nsha256 = \"{}\"\nstrip_components = 2\n",
        sha256_hex(&archive)
    ))
    .unwrap();
    let (_root, staging, cache) = work_dirs();
    let error = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap_err();
    assert!(error.to_string().contains("duplicate member path"), "{error}");
}

#[test]
fn assembling_switches_the_current_link_and_verifies() {
    let archive = tar_gz(&[("root/app", b"bin", 0o755, b'0')]);
    let url = artifact_url("app.tar.gz");
    let sums = fake_acquire(&[("app.tar.gz", archive.clone())]);
    let manifest = manifest_for(&[("app", &url, &sha256_hex(&archive))]);
    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("staging");
    let cache = root.path().join("cache");
    let site_dir = root.path().join("site");
    let built = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    let first = mymirror::assemble(&site_dir, &built).unwrap();
    assert!(!first);
    let current = std::fs::read_link(site_dir.join("current")).unwrap();
    assert_eq!(current, Path::new("generations").join(&built.generation));
    assert!(site_dir.join("generations").join(&built.generation).join("app.zip").is_file());
    assert!(site_dir.join("generations").join(&built.generation).join("catalog.json").is_file());
    assert!(site_dir.join("generations").join(&built.generation).join("SHA256SUMS").is_file());
    let second = mymirror::assemble(&site_dir, &built).unwrap();
    assert!(second);
}

#[test]
fn a_zip_with_upstream_layout_is_supported() {
    let mut encoder_out = Vec::new();
    {
        let file = std::io::Cursor::new(&mut encoder_out);
        let mut archive = rawzip::ZipArchiveWriter::new(file);
        let (mut entry, config) = archive
            .new_file("opencode")
            .compression_method(rawzip::CompressionMethod::DEFLATE)
            .unix_permissions(0o100755)
            .start()
            .unwrap();
        let encoder = flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
        let mut writer = config.wrap(encoder);
        writer.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        let (encoder, descriptor) = writer.finish().unwrap();
        encoder.finish().unwrap();
        entry.finish(descriptor).unwrap();
        archive.finish().unwrap();
    }
    let url = artifact_url("opencode-test.zip");
    let sums = fake_acquire(&[("opencode-test.zip", encoder_out.clone())]);
    let manifest = manifest_for(&[("opencode-test", &url, &sha256_hex(&encoder_out))]);
    let (_root, staging, cache) = work_dirs();
    let built = build(&manifest, "https://mirror.example.test", &sums, Some(&cache), &staging).unwrap();
    assert_eq!(built.artifacts[0].files, vec!["opencode"]);
}
