#![forbid(unsafe_code)]

use crate::error::Error;
use deser_value::Value;
use std::collections::BTreeMap;

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn quoted(text: &str) -> String {
    format!("\"{}\"", escape(text))
}

pub(crate) fn parse(text: &str, context: &str) -> Result<Value, Error> {
    deser_json::from_str(text)
        .map_err(|e| Error::manifest_invalid(format!("{context} is not valid JSON: {e}")))
}

pub(crate) fn string_field(map: &deser_value::Map, key: &str, context: &str) -> Result<String, Error> {
    map.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::manifest_invalid(format!("{context}.{key} must be a string")))
}

pub(crate) fn optional_string_field(
    map: &deser_value::Map,
    key: &str,
    context: &str,
) -> Result<Option<String>, Error> {
    match map.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| Error::manifest_invalid(format!("{context}.{key} must be a string"))),
    }
}

pub(crate) fn string_map(
    value: &Value,
    context: &str,
) -> Result<BTreeMap<String, String>, Error> {
    let map = value
        .as_map()
        .ok_or_else(|| Error::manifest_invalid(format!("{context} must be an object")))?;
    let mut out = BTreeMap::new();
    for (key, value) in map.iter() {
        let key = key
            .as_str()
            .ok_or_else(|| Error::manifest_invalid(format!("{context} keys must be strings")))?;
        let value = value
            .as_str()
            .ok_or_else(|| Error::manifest_invalid(format!("{context}.{key} must be a string")))?;
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json_strings() {
        assert_eq!(quoted("a\"b\\c\n\t\u{1}"), "\"a\\\"b\\\\c\\n\\t\\u0001\"");
    }
}
