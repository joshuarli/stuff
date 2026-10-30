#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::collections::BTreeMap;

pub fn as_map(v: &deser_value::Value) -> Option<&deser_value::Map> {
    v.as_map()
}

pub fn req_map<'a>(v: &'a deser_value::Value, label: &str) -> Result<&'a deser_value::Map> {
    v.as_map()
        .ok_or_else(|| Error::Input(format!("{label} must be an object")))
}

pub fn req_str(v: &deser_value::Value, label: &str) -> Result<String> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s.trim() == s => Ok(s.to_string()),
        _ => Err(Error::Input(format!("{label} must be a non-empty string"))),
    }
}

pub fn opt_str(v: &deser_value::Value) -> Option<String> {
    if matches!(&**v, deser_value::Kind::Null) {
        return None;
    }
    v.as_str().map(|s| s.to_string())
}

pub fn map_keys(map: &deser_value::Map) -> Vec<String> {
    let mut out = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let Some((k, _)) = map.get_index(i)
            && let Some(s) = k.as_str() {
                out.push(s.to_string());
            }
    }
    out
}

pub fn is_null(v: &deser_value::Value) -> bool {
    matches!(&**v, deser_value::Kind::Null)
}

fn valid_name_char(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
}

fn valid_name_part(part: &str) -> bool {
    let b = part.as_bytes();
    !part.is_empty()
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|&c| valid_name_char(c))
}

pub fn valid_package_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix('@') {
        match rest.find('/') {
            Some(i) => {
                let (scope, pkg) = (&rest[..i], &rest[i + 1..]);
                !scope.is_empty()
                    && !pkg.is_empty()
                    && !pkg.contains('/')
                    && valid_name_part(scope)
                    && valid_name_part(pkg)
            }
            None => false,
        }
    } else {
        !name.contains('/') && valid_name_part(name)
    }
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s.len() == 1 || !s.starts_with('0'))
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

pub fn valid_version(version: &str) -> bool {
    let mut rest = version;
    for _ in 0..2 {
        match rest.find('.') {
            Some(i) => {
                if !is_number(&rest[..i]) {
                    return false;
                }
                rest = &rest[i + 1..];
            }
            None => return false,
        }
    }
    let patch_end = rest.find(['-', '+']).unwrap_or(rest.len());
    if !is_number(&rest[..patch_end]) {
        return false;
    }
    let mut tail = &rest[patch_end..];
    if let Some(stripped) = tail.strip_prefix('-') {
        let end = stripped.find('+').unwrap_or(stripped.len());
        if !valid_ident(&stripped[..end]) {
            return false;
        }
        tail = &stripped[end..];
    }
    if let Some(stripped) = tail.strip_prefix('+') {
        if !valid_ident(stripped) {
            return false;
        }
    } else if !tail.is_empty() {
        return false;
    }
    true
}

pub fn integrity_lengths(algo: &str) -> Option<usize> {
    match algo {
        "sha1" => Some(20),
        "sha256" => Some(32),
        "sha384" => Some(48),
        "sha512" => Some(64),
        _ => None,
    }
}

pub fn check_integrity_token(algo: &str, digest: &str, label: &str) -> Result<String> {
    let len = integrity_lengths(algo).ok_or_else(|| {
        Error::Input(format!("{label} contains an unsupported integrity token"))
    })?;
    if digest.is_empty()
        || !digest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    {
        return Err(Error::Input(format!(
            "{label} contains an unsupported integrity token"
        )));
    }
    let bytes = B64.decode(digest).map_err(|_| {
        Error::Input(format!("{label} contains a malformed {algo} digest"))
    })?;
    if bytes.len() != len || B64.encode(&bytes) != digest {
        return Err(Error::Input(format!(
            "{label} contains a malformed {algo} digest"
        )));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn check_integrity_string(text: &str, label: &str) -> Result<BTreeMap<String, String>> {
    if text.trim().is_empty() {
        return Err(Error::Input(format!("{label} is malformed")));
    }
    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for token in text.split_whitespace() {
        let claim = token.split('?').next().unwrap_or("");
        let Some(sep) = claim.find('-') else {
            return Err(Error::Input(format!("{label} contains an unsupported integrity token")));
        };
        let (algo, digest) = (&claim[..sep], &claim[sep + 1..]);
        let hex = check_integrity_token(algo, digest, label)?;
        if let Some(prev) = claims.get(algo)
            && prev != &hex {
                return Err(Error::Input(format!("{label} contains conflicting {algo} claims")));
            }
        claims.insert(algo.to_string(), hex);
    }
    Ok(claims)
}

pub fn lenient_hex(bytes_text: &str) -> String {
    let filtered: String = bytes_text
        .bytes()
        .filter(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/' || *b == b'=')
        .map(|b| b as char)
        .collect();
    B64.decode(filtered)
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
        .unwrap_or_default()
}

pub fn is_sha1_hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn is_sha512_hex(s: &str) -> bool {
    s.len() == 128 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn valid_manager_pin(pin: &str, manager: &str) -> bool {
    let Some((mgr_part, rest)) = pin.split_once('@') else {
        return false;
    };
    if mgr_part != manager {
        return false;
    }
    let (ver_part, build) = match rest.split_once('+') {
        Some((v, b)) => (v, Some(b)),
        None => (rest, None),
    };
    if !valid_version(ver_part) {
        return false;
    }
    match build {
        None => true,
        Some(b) => b.strip_prefix("sha512.").is_some_and(|d| {
            !d.is_empty()
                && d.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=' || c == b'_' || c == b'-')
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions() {
        assert!(valid_package_name("@scope/pkg"));
        assert!(valid_package_name("pkg"));
        assert!(!valid_package_name("@scope"));
        assert!(!valid_package_name("Pkg"));
        assert!(valid_version("1.2.3"));
        assert!(valid_version("1.2.3-alpha.1+build.5"));
        assert!(!valid_version("1.2"));
        assert!(!valid_version("01.2.3"));
    }

    #[test]
    fn integrity_tokens() {
        let err = check_integrity_token("sha512", &"A".repeat(86), "x").unwrap_err();
        assert!(format!("{err}").contains("malformed"));
        assert!(check_integrity_token("md5", "abcd", "x").is_err());
        let zeros = format!("sha512-{}==", "A".repeat(86));
        let ok = check_integrity_string(&zeros, "x").unwrap();
        assert_eq!(ok.get("sha512").map(|s| s.len()), Some(128));
    }

    #[test]
    fn pins() {
        assert!(valid_manager_pin("npm@1.2.3", "npm"));
        assert!(valid_manager_pin("pnpm@10.2.0+sha512.abc_def-12+/=", "pnpm"));
        assert!(!valid_manager_pin("npm@1.2.3", "pnpm"));
        assert!(!valid_manager_pin("npm@1.2", "npm"));
    }
}
