use mycrates::crate_file::{scan_crate, ScanLimits};
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::Builder;

fn make_crate(name: &str, ver: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = format!("[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n", name, ver);
    let mut header = tar::Header::new_gnu();
    let path = format!("{}-{}/Cargo.toml", name, ver);
    header.set_path(&path).unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();
    let _ = tar.into_inner().unwrap();
    encoder.finish().unwrap()
}

#[test]
fn good_crate() {
    let data = make_crate("foo", "1.0.0");
    let res = scan_crate(&data[..], "foo", "1.0.0", &ScanLimits::default()).unwrap();
    assert!(!res.checksum.is_empty());
}

#[test]
fn bad_prefix() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n";
    let mut header = tar::Header::new_gnu();
    header.set_path("wrong/Cargo.toml").unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();
    let _ = tar.into_inner().unwrap();
    let data = encoder.finish().unwrap();
    let err = scan_crate(&data[..], "foo", "1.0.0", &ScanLimits::default()).unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("integrity"));
}

#[test]
fn duplicate_paths_rejected() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n";
    for _ in 0..2 {
        let mut header = tar::Header::new_gnu();
        header.set_path("foo-1.0.0/Cargo.toml").unwrap();
        header.set_size(cargo_toml.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, cargo_toml.as_bytes()).unwrap();
    }
    let _ = tar.into_inner().unwrap();
    let data = encoder.finish().unwrap();
    let err = scan_crate(&data[..], "foo", "1.0.0", &ScanLimits::default()).unwrap_err();
    assert!(format!("{err}").contains("duplicate"));
}

#[test]
fn unsupported_entry_type_rejected() {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\nedition = \"2021\"\n";
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_path("foo-1.0.0/link").unwrap();
    header.set_link_name("target").unwrap();
    header.set_size(0);
    header.set_mode(0o777);
    header.set_cksum();
    tar.append(&header, &[][..]).unwrap();
    let mut header2 = tar::Header::new_gnu();
    header2.set_path("foo-1.0.0/Cargo.toml").unwrap();
    header2.set_size(cargo_toml.len() as u64);
    header2.set_mode(0o644);
    header2.set_cksum();
    tar.append(&header2, cargo_toml.as_bytes()).unwrap();
    let _ = tar.into_inner().unwrap();
    let data = encoder.finish().unwrap();
    let err = scan_crate(&data[..], "foo", "1.0.0", &ScanLimits::default()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("unsupported entry type"), "got: {msg}");
}

#[test]
fn bounded_reader_prevents_unbounded_allocation() {
    let limits = ScanLimits {
        max_compressed: 50,
        ..ScanLimits::default()
    };
    let infinite_zeros = std::io::repeat(0);
    let err = scan_crate(infinite_zeros, "foo", "1.0.0", &limits).unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("compressed size exceeded"));
}
