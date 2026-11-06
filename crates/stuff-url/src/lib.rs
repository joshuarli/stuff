#![forbid(unsafe_code)]

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlError {
    MissingScheme,
    MissingAuthority,
    InvalidHost,
    InvalidPort,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingScheme => "relative URL without a base",
            Self::MissingAuthority => "URL has no // authority",
            Self::InvalidHost => "invalid host",
            Self::InvalidPort => "invalid port",
        })
    }
}

impl std::error::Error for UrlError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    scheme: String,
    username: String,
    password: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
    serialization: String,
}

fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn clean(input: &str) -> String {
    input
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

fn split_scheme(input: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = input.split_once(':')?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some((scheme, rest))
}

fn encode(input: &str, escape: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if byte <= 0x20 || byte >= 0x7f || escape(byte) {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(byte as char);
        }
    }
    out
}

fn encode_userinfo(input: &str) -> String {
    encode(input, |b| b"\"#<>?`{}/:;=@[\\]^|".contains(&b))
}

fn encode_path(input: &str) -> String {
    encode(input, |b| b"\"#<>?`{}".contains(&b))
}

fn encode_query(input: &str, special: bool) -> String {
    encode(input, |b| b"\"#<>".contains(&b) || (special && b == b'\''))
}

fn encode_fragment(input: &str) -> String {
    encode(input, |b| b"\"<>`".contains(&b))
}

fn is_single_dot(segment: &str) -> bool {
    segment == "." || segment.eq_ignore_ascii_case("%2e")
}

fn is_double_dot(segment: &str) -> bool {
    matches!(segment.to_ascii_lowercase().as_str(), ".." | ".%2e" | "%2e." | "%2e%2e")
}

fn resolve_dots(path: &str) -> String {
    let Some(tail) = path.strip_prefix('/') else {
        return path.to_string();
    };
    let segments: Vec<&str> = tail.split('/').collect();
    let mut out: Vec<&str> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        if is_double_dot(segment) {
            out.pop();
            if last {
                out.push("");
            }
        } else if is_single_dot(segment) {
            if last {
                out.push("");
            }
        } else {
            out.push(segment);
        }
    }
    format!("/{}", out.join("/"))
}

fn ends_in_number(host: &str) -> bool {
    let last = host.strip_suffix('.').unwrap_or(host).rsplit('.').next().unwrap_or("");
    if last.is_empty() {
        return false;
    }
    last.bytes().all(|b| b.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn format_ipv6(address: Ipv6Addr) -> String {
    let segments = address.segments();
    let mut best: Option<(usize, usize)> = None;
    let mut index = 0;
    while index < 8 {
        if segments[index] == 0 {
            let start = index;
            while index < 8 && segments[index] == 0 {
                index += 1;
            }
            let length = index - start;
            if length >= 2 && best.is_none_or(|(_, best_length)| length > best_length) {
                best = Some((start, length));
            }
        } else {
            index += 1;
        }
    }
    let hex = |range: &[u16]| range.iter().map(|segment| format!("{segment:x}")).collect::<Vec<_>>().join(":");
    match best {
        Some((start, length)) => format!("{}::{}", hex(&segments[..start]), hex(&segments[start + length..])),
        None => hex(&segments),
    }
}

fn parse_host(raw: &str, scheme: &str) -> Result<Option<String>, UrlError> {
    if let Some(inner) = raw.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or(UrlError::InvalidHost)?;
        let address: Ipv6Addr = inner.parse().map_err(|_| UrlError::InvalidHost)?;
        return Ok(Some(format!("[{}]", format_ipv6(address))));
    }
    let host = raw.to_ascii_lowercase();
    let forbidden = |c: char| {
        !c.is_ascii()
            || c.is_ascii_control()
            || matches!(c, ' ' | '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|' | '%')
    };
    if host.chars().any(forbidden) {
        return Err(UrlError::InvalidHost);
    }
    if host.is_empty() {
        return if matches!(scheme, "file") || !is_special(scheme) {
            Ok(None)
        } else {
            Err(UrlError::InvalidHost)
        };
    }
    if ends_in_number(&host) {
        let address: Ipv4Addr = host.parse().map_err(|_| UrlError::InvalidHost)?;
        return Ok(Some(address.to_string()));
    }
    if scheme == "file" && host == "localhost" {
        return Ok(None);
    }
    Ok(Some(host))
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, UrlError> {
        let input = clean(input);
        let (scheme, rest) = split_scheme(&input).ok_or(UrlError::MissingScheme)?;
        let scheme = scheme.to_ascii_lowercase();
        let special = is_special(&scheme);
        let rest = if special { rest.replace('\\', "/") } else { rest.to_string() };
        let after = rest.strip_prefix("//").ok_or(UrlError::MissingAuthority)?;

        let (before_fragment, fragment) = match after.split_once('#') {
            Some((head, tail)) => (head, Some(encode_fragment(tail))),
            None => (after, None),
        };
        let (before_query, query) = match before_fragment.split_once('?') {
            Some((head, tail)) => (head, Some(encode_query(tail, special))),
            None => (before_fragment, None),
        };
        let (authority, raw_path) = match before_query.find('/') {
            Some(index) => before_query.split_at(index),
            None => (before_query, ""),
        };

        let (userinfo, host_port) = match authority.rsplit_once('@') {
            Some((userinfo, host_port)) => (Some(userinfo), host_port),
            None => (None, authority),
        };
        let (username, password) = match userinfo {
            Some(userinfo) => match userinfo.split_once(':') {
                Some((name, secret)) => (encode_userinfo(name), Some(encode_userinfo(secret))),
                None => (encode_userinfo(userinfo), None),
            },
            None => (String::new(), None),
        };
        let password = password.filter(|secret| !secret.is_empty());

        let (raw_host, raw_port) = if host_port.starts_with('[') {
            let end = host_port.find(']').ok_or(UrlError::InvalidHost)?;
            let (host, tail) = host_port.split_at(end + 1);
            match tail {
                "" => (host, None),
                _ => (host, Some(tail.strip_prefix(':').ok_or(UrlError::InvalidHost)?)),
            }
        } else {
            match host_port.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (host_port, None),
            }
        };
        let host = parse_host(raw_host, &scheme)?;
        let port = match raw_port {
            None | Some("") => None,
            Some(digits) => {
                if !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(UrlError::InvalidPort);
                }
                let port: u16 = digits.parse().map_err(|_| UrlError::InvalidPort)?;
                (default_port(&scheme) != Some(port)).then_some(port)
            }
        };

        let mut path = resolve_dots(&encode_path(raw_path));
        if special && path.is_empty() {
            path.push('/');
        }
        let mut url = Self { scheme, username, password, host, port, path, query, fragment, serialization: String::new() };
        url.refresh();
        Ok(url)
    }

    pub fn join(&self, location: &str) -> Result<Self, UrlError> {
        let location = clean(location);
        if split_scheme(&location).is_some() {
            return Self::parse(&location);
        }
        let special = is_special(&self.scheme);
        let location = if special { location.replace('\\', "/") } else { location };
        if location.starts_with("//") {
            return Self::parse(&format!("{}:{location}", self.scheme));
        }
        let origin = format!("{}:{}", self.scheme, self.authority_prefix());
        let target = if location.is_empty() {
            format!("{origin}{}{}", self.path, self.query_suffix())
        } else if location.starts_with('#') {
            format!("{origin}{}{}{location}", self.path, self.query_suffix())
        } else if location.starts_with('?') {
            format!("{origin}{}{location}", self.path)
        } else if location.starts_with('/') {
            format!("{origin}{location}")
        } else {
            let directory = &self.path[..self.path.rfind('/').map_or(0, |index| index + 1)];
            let directory = if directory.is_empty() { "/" } else { directory };
            format!("{origin}{directory}{location}")
        };
        Self::parse(&target)
    }

    fn authority_prefix(&self) -> String {
        let mut out = String::from("//");
        if !self.username.is_empty() || self.password.is_some() {
            out.push_str(&self.username);
            if let Some(password) = &self.password {
                out.push(':');
                out.push_str(password);
            }
            out.push('@');
        }
        if let Some(host) = &self.host {
            out.push_str(host);
        }
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(&port.to_string());
        }
        out
    }

    fn query_suffix(&self) -> String {
        self.query.as_ref().map(|query| format!("?{query}")).unwrap_or_default()
    }

    fn refresh(&mut self) {
        let mut out = format!("{}:{}{}{}", self.scheme, self.authority_prefix(), self.path, self.query_suffix());
        if let Some(fragment) = &self.fragment {
            out.push('#');
            out.push_str(fragment);
        }
        self.serialization = out;
    }

    pub fn clear_credentials(&mut self) {
        self.username.clear();
        self.password = None;
        self.refresh();
    }

    pub fn clear_query(&mut self) {
        self.query = None;
        self.refresh();
    }

    pub fn clear_fragment(&mut self) {
        self.fragment = None;
        self.refresh();
    }

    pub fn as_str(&self) -> &str {
        &self.serialization
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    pub fn host_str(&self) -> Option<&str> {
        self.host.as_deref()
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    pub fn origin_ascii(&self) -> String {
        match (&self.host, self.scheme.as_str()) {
            (Some(host), "http" | "https" | "ws" | "wss" | "ftp") => match self.port {
                Some(port) => format!("{}://{host}:{port}", self.scheme),
                None => format!("{}://{host}", self.scheme),
            },
            _ => "null".to_string(),
        }
    }
}

impl FromStr for Url {
    type Err = UrlError;

    fn from_str(input: &str) -> Result<Self, UrlError> {
        Self::parse(input)
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialization)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(input: &str) -> String {
        Url::parse(input).expect("parses").to_string()
    }

    #[test]
    fn normalizes_scheme_host_default_port_and_empty_path() {
        assert_eq!(normalized("HTTPS://Registry.NPMJS.org:443"), "https://registry.npmjs.org/");
        assert_eq!(normalized("http://[1:0:0:0:0:0:0:1]:8080/p"), "http://[1::1]:8080/p");
        assert_eq!(normalized("https://h.test/a/../b/./c/%2E%2e/d"), "https://h.test/b/d");
        assert_eq!(normalized("https://h.test/a b?q 1#f g"), "https://h.test/a%20b?q%201#f%20g");
    }

    #[test]
    fn exposes_authority_parts() {
        let url = Url::parse("https://us%40er:p@h.test:8443/a?q=1#f").unwrap();
        assert_eq!(url.username(), "us%40er");
        assert_eq!(url.password(), Some("p"));
        assert_eq!(url.host_str(), Some("h.test"));
        assert_eq!(url.port(), Some(8443));
        assert_eq!((url.query(), url.fragment()), (Some("q=1"), Some("f")));
        assert_eq!(Url::parse("https://u:@h.test/").unwrap().password(), None);
        assert_eq!(Url::parse("https://h.test/?").unwrap().query(), Some(""));
        assert_eq!(Url::parse("file:///tmp/x").unwrap().host_str(), None);
        assert_eq!(Url::parse("file://localhost/tmp/x").unwrap().host_str(), None);
    }

    #[test]
    fn origin_is_null_for_non_network_schemes() {
        assert_eq!(Url::parse("http://127.0.0.1:8080/x").unwrap().origin_ascii(), "http://127.0.0.1:8080");
        assert_eq!(Url::parse("https://h.test:443/x").unwrap().origin_ascii(), "https://h.test");
        assert_eq!(Url::parse("file:///x").unwrap().origin_ascii(), "null");
        assert_eq!(Url::parse("git+ssh://git@h.test/r").unwrap().origin_ascii(), "null");
    }

    #[test]
    fn keeps_non_special_urls_opaque_to_defaults() {
        assert_eq!(normalized("git+ssh://git@github.com/a/b.git#abc"), "git+ssh://git@github.com/a/b.git#abc");
        assert_eq!(Url::parse("foo://").unwrap().path(), "");
    }

    #[test]
    fn rejects_what_it_cannot_represent_faithfully() {
        for input in [
            "not a url",
            "mailto:a@b",
            "//h/x",
            "https:///x",
            "https://h.test:99999/",
            "https://h.test:abc/",
            "https://exa mple.com/",
            "https://\u{fc}.test/",
            "https://127.1/",
            "https://0x7f.0.0.1/",
            "https://1.2.3.256/",
            "https://[::1/",
        ] {
            assert!(Url::parse(input).is_err(), "{input}");
        }
    }

    #[test]
    fn joins_like_a_redirect_location() {
        let base = Url::parse("https://registry.npmjs.org/a/b/c.tgz?x=1#frag").unwrap();
        let cases = [
            ("", "https://registry.npmjs.org/a/b/c.tgz?x=1"),
            ("#f", "https://registry.npmjs.org/a/b/c.tgz?x=1#f"),
            ("?q", "https://registry.npmjs.org/a/b/c.tgz?q"),
            ("/abs", "https://registry.npmjs.org/abs"),
            ("rel?q#f", "https://registry.npmjs.org/a/b/rel?q#f"),
            ("../up", "https://registry.npmjs.org/a/up"),
            ("../../../x", "https://registry.npmjs.org/x"),
            ("//other.test/p", "https://other.test/p"),
            ("http://abs.test/z", "http://abs.test/z"),
        ];
        for (location, expected) in cases {
            assert_eq!(base.join(location).unwrap().to_string(), expected, "{location}");
        }
        let with_credentials = Url::parse("https://u:p@h.test:8443/a/b").unwrap();
        assert_eq!(with_credentials.join("c").unwrap().to_string(), "https://u:p@h.test:8443/a/c");
        assert_eq!(Url::parse("https://h.test").unwrap().join("x").unwrap().to_string(), "https://h.test/x");
    }

    #[test]
    fn strips_credentials_query_and_fragment() {
        let mut url = Url::parse("https://u:p@h.test/a?q=1#f").unwrap();
        url.clear_credentials();
        url.clear_query();
        url.clear_fragment();
        assert_eq!(url.to_string(), "https://h.test/a");
    }
}
