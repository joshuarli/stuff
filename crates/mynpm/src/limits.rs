#![forbid(unsafe_code)]

pub const METADATA_BYTES: u64 = 64 * 1024 * 1024;
pub const PACKAGE_REPORT_BYTES: u64 = 64 * 1024 * 1024;
pub const TARBALL_BYTES: u64 = 1024 * 1024 * 1024;
pub const UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const ARCHIVE_ENTRIES: usize = 100_000;
pub const REDIRECTS: u32 = 5;
pub const REQUEST_MILLISECONDS: u64 = 10 * 60 * 1000;
pub const FETCH_MILLISECONDS: u64 = 60 * 60 * 1000;
pub const CONCURRENT_DOWNLOADS: usize = 4;
pub const FETCH_BYTES: u64 = 10 * 1024 * 1024 * 1024;

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[(&str, u64)] = &[("GiB", 1024 * 1024 * 1024), ("MiB", 1024 * 1024), ("KiB", 1024)];
    for (unit, size) in UNITS {
        if bytes >= *size && bytes.is_multiple_of(*size) {
            return format!("{} {unit}", bytes / size);
        }
    }
    format!("{bytes} bytes")
}

pub fn format_duration(milliseconds: u64) -> String {
    if milliseconds.is_multiple_of(3_600_000) {
        let n = milliseconds / 3_600_000;
        return if n == 1 {
            "1 hour".to_string()
        } else {
            format!("{n} hours")
        };
    }
    if milliseconds.is_multiple_of(60_000) {
        let n = milliseconds / 60_000;
        return if n == 1 {
            "1 minute".to_string()
        } else {
            format!("{n} minutes")
        };
    }
    if milliseconds.is_multiple_of(1_000) {
        let n = milliseconds / 1_000;
        return if n == 1 {
            "1 second".to_string()
        } else {
            format!("{n} seconds")
        };
    }
    format!("{milliseconds} milliseconds")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_and_duration_units() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1 GiB");
        assert_eq!(format_bytes(64 * 1024 * 1024), "64 MiB");
        assert_eq!(format_bytes(7), "7 bytes");
        assert_eq!(format_duration(3_600_000), "1 hour");
        assert_eq!(format_duration(60_000), "1 minute");
        assert_eq!(format_duration(1_000), "1 second");
        assert_eq!(format_duration(500), "500 milliseconds");
    }

    #[test]
    fn frozen_caps_match_typescript() {
        assert_eq!(METADATA_BYTES, 64 * 1024 * 1024);
        assert_eq!(TARBALL_BYTES, 1024 * 1024 * 1024);
        assert_eq!(UNPACKED_BYTES, 2 * 1024 * 1024 * 1024);
        assert_eq!(ARCHIVE_ENTRIES, 100_000);
        assert_eq!(REDIRECTS, 5);
        assert_eq!(CONCURRENT_DOWNLOADS, 4);
        assert_eq!(FETCH_BYTES, 10 * 1024 * 1024 * 1024);
    }
}
