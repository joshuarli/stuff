#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const SERVICE: &str = "s3";
const TERMINATOR: &str = "aws4_request";

#[derive(Debug, Clone, Copy)]
pub struct SigningMaterial<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub session_token: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedHeaders {
    pub headers: Vec<(String, String)>,
    pub authorization: String,
}

#[derive(Debug, Clone, Copy)]
pub struct SignRequest<'a> {
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub payload_hash: &'a str,
    pub content_type: Option<&'a str>,
    pub extra_headers: &'a [(&'a str, &'a str)],
    pub amz_date: &'a str,
    pub datestamp: &'a str,
}

pub fn hex(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes))
}

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(message);
    let digest = mac.finalize().into_bytes();
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    hex(hmac_sha256(key, message))
}

pub fn quote_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                write!(out, "%{byte:02X}").expect("writing to a string cannot fail");
            }
        }
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(high * 16 + low);
                        index += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn canonical_query(query: &str) -> String {
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (quote_component(&percent_decode(key)), quote_component(&percent_decode(value)))
        })
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn normalize_header(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn canonical_request(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
) -> (String, String) {
    let mut sorted: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), normalize_header(value)))
        .collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_headers: String = sorted
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = sorted
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let path = if path.is_empty() { "/" } else { path };
    let canonical = format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        canonical_query(query)
    );
    (canonical, signed_headers)
}

pub fn credential_scope(datestamp: &str, region: &str, service: &str) -> String {
    format!("{datestamp}/{region}/{service}/{TERMINATOR}")
}

pub fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    )
}

pub fn signing_key(secret: &str, datestamp: &str, region: &str, service: &str) -> [u8; 32] {
    let date = hmac_sha256(format!("AWS4{secret}").as_bytes(), datestamp.as_bytes());
    let regional = hmac_sha256(&date, region.as_bytes());
    let scoped = hmac_sha256(&regional, service.as_bytes());
    hmac_sha256(&scoped, TERMINATOR.as_bytes())
}

pub fn sign_request(material: SigningMaterial<'_>, request: &SignRequest<'_>) -> SignedHeaders {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    headers.insert("host".to_string(), request.host.to_string());
    headers.insert(
        "x-amz-content-sha256".to_string(),
        request.payload_hash.to_string(),
    );
    headers.insert("x-amz-date".to_string(), request.amz_date.to_string());
    if let Some(content_type) = request.content_type
        && !content_type.is_empty()
    {
        headers.insert("content-type".to_string(), content_type.to_string());
    }
    if let Some(token) = material.session_token
        && !token.is_empty()
    {
        headers.insert("x-amz-security-token".to_string(), token.to_string());
    }
    for (name, value) in request.extra_headers {
        headers.insert(name.to_ascii_lowercase(), (*value).to_string());
    }
    let list: Vec<(String, String)> = headers.into_iter().collect();
    let (canonical, signed_headers) = canonical_request(
        request.method,
        request.path,
        request.query,
        &list,
        request.payload_hash,
    );
    let scope = credential_scope(request.datestamp, material.region, SERVICE);
    let signature = hmac_sha256_hex(
        &signing_key(
            material.secret_access_key,
            request.datestamp,
            material.region,
            SERVICE,
        ),
        string_to_sign(request.amz_date, &scope, &canonical).as_bytes(),
    );
    let authorization = format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        material.access_key_id
    );
    SignedHeaders { headers: list, authorization }
}

pub fn format_amz_date(epoch_seconds: u64) -> (String, String) {
    let days = (epoch_seconds / 86_400) as i64;
    let seconds = epoch_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;
    let datestamp = format!("{year:04}{month:02}{day:02}");
    let amz_date = format!("{datestamp}T{hour:02}{minute:02}{second:02}Z");
    (datestamp, amz_date)
}

pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096)
            / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCESS: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn material<'a>(session_token: Option<&'a str>) -> SigningMaterial<'a> {
        SigningMaterial {
            access_key_id: ACCESS,
            secret_access_key: SECRET,
            region: "us-east-1",
            session_token,
        }
    }

    #[test]
    fn matches_the_aws_get_object_vector_and_the_python_suite() {
        let signed = sign_request(
            material(None),
            &SignRequest {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                query: "",
                payload_hash: EMPTY_SHA256,
                content_type: None,
                extra_headers: &[("range", "bytes=0-9")],
                amz_date: "20130524T000000Z",
                datestamp: "20130524",
            },
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn matches_the_aws_put_object_vector() {
        let payload_hash =
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072";
        let signed = sign_request(
            material(None),
            &SignRequest {
                method: "PUT",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test%24file.text",
                query: "",
                payload_hash,
                content_type: None,
                extra_headers: &[
                    ("date", "Fri, 24 May 2013 00:00:00 GMT"),
                    ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
                ],
                amz_date: "20130524T000000Z",
                datestamp: "20130524",
            },
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class, \
             Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn matches_the_aws_test_suite_get_vanilla_vector() {
        let headers = vec![
            ("host".to_string(), "example.amazonaws.com".to_string()),
            ("x-amz-date".to_string(), "20150830T123600Z".to_string()),
        ];
        let (canonical, signed) =
            canonical_request("GET", "/", "", &headers, EMPTY_SHA256);
        assert_eq!(signed, "host;x-amz-date");
        let scope = credential_scope("20150830", "us-east-1", "service");
        let signature = hmac_sha256_hex(
            &signing_key(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "20150830",
                "us-east-1",
                "service",
            ),
            string_to_sign("20150830T123600Z", &scope, &canonical).as_bytes(),
        );
        assert_eq!(
            signature,
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn matches_the_aws_test_suite_query_order_vector() {
        let headers = vec![
            ("host".to_string(), "example.amazonaws.com".to_string()),
            ("x-amz-date".to_string(), "20150830T123600Z".to_string()),
        ];
        let (canonical, _) = canonical_request(
            "GET",
            "/",
            "Param2=value2&Param1=value1",
            &headers,
            EMPTY_SHA256,
        );
        assert!(canonical.contains("\nParam1=value1&Param2=value2\n"));
        let scope = credential_scope("20150830", "us-east-1", "service");
        let signature = hmac_sha256_hex(
            &signing_key(
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                "20150830",
                "us-east-1",
                "service",
            ),
            string_to_sign("20150830T123600Z", &scope, &canonical).as_bytes(),
        );
        assert_eq!(
            signature,
            "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        );
    }

    #[test]
    fn canonicalizes_an_empty_query_value_as_uploads_equals() {
        let headers = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("x-amz-content-sha256".to_string(), EMPTY_SHA256.to_string()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let (canonical, signed) =
            canonical_request("POST", "/bucket/key", "uploads", &headers, EMPTY_SHA256);
        assert!(canonical.contains("\nuploads=\n"), "{canonical}");
        assert_eq!(signed, "host;x-amz-content-sha256;x-amz-date");
    }

    #[test]
    fn part_upload_sorts_content_type_before_amz_headers() {
        let headers = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("x-amz-content-sha256".to_string(), "a".repeat(64)),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
            ("content-type".to_string(), "application/octet-stream".to_string()),
        ];
        let (canonical, signed) = canonical_request(
            "PUT",
            "/bucket/key",
            "partNumber=1&uploadId=abc",
            &headers,
            &"a".repeat(64),
        );
        assert!(canonical.contains("\npartNumber=1&uploadId=abc\n"), "{canonical}");
        assert_eq!(signed, "content-type;host;x-amz-content-sha256;x-amz-date");
        assert!(
            canonical.contains(
                "content-type:application/octet-stream\nhost:examplebucket.s3.amazonaws.com\n"
            ),
            "{canonical}"
        );
    }

    #[test]
    fn completion_canonicalizes_the_query_in_sorted_order() {
        let headers = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("x-amz-content-sha256".to_string(), "a".repeat(64)),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
            ("content-type".to_string(), "text/plain".to_string()),
        ];
        let (canonical, _) = canonical_request(
            "POST",
            "/bucket/key",
            "uploadId=abc",
            &headers,
            &"a".repeat(64),
        );
        assert!(canonical.contains("\nuploadId=abc\n"), "{canonical}");
    }

    #[test]
    fn session_token_is_signed_and_sent() {
        let signed = sign_request(
            material(Some("temporary-token")),
            &SignRequest {
                method: "POST",
                host: "examplebucket.s3.amazonaws.com",
                path: "/bucket/key",
                query: "uploads",
                payload_hash: EMPTY_SHA256,
                content_type: None,
                extra_headers: &[],
                amz_date: "20130524T000000Z",
                datestamp: "20130524",
            },
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token, \
             Signature=99df8cec29472428804d3ac294eaf579155853626eb8c53ff7306846ca96efaf"
        );
        assert_eq!(
            signed
                .headers
                .iter()
                .find(|(name, _)| name == "x-amz-security-token")
                .map(|(_, value)| value.as_str()),
            Some("temporary-token")
        );
    }

    #[test]
    fn queries_decode_then_reencode_and_sort() {
        assert_eq!(canonical_query("b=2&a=1&a="), "a=&a=1&b=2");
        assert_eq!(canonical_query("prefix=a%20b/c"), "prefix=a%20b%2Fc");
        assert_eq!(canonical_query(""), "");
    }

    #[test]
    fn quoting_matches_python_quote_with_safe_unreserved() {
        assert_eq!(
            quote_component("a b/c$d~e_f.g-h"),
            "a%20b%2Fc%24d~e_f.g-h"
        );
    }

    #[test]
    fn formats_the_amz_date_in_utc() {
        assert_eq!(
            format_amz_date(1_369_353_600),
            ("20130524".to_string(), "20130524T000000Z".to_string())
        );
        assert_eq!(
            format_amz_date(1_369_357_261),
            ("20130524".to_string(), "20130524T010101Z".to_string())
        );
    }
}
