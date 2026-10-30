#![forbid(unsafe_code)]

use crate::error::{Error, Result};

pub type Value = deser_value::Value;

const PRETTY: deser_json::SerializerConfig = deser_json::SerializerConfig::builder()
    .pretty(deser_json::Indent::Spaces(2))
    .build();

pub fn from_str<'de, T: deser::Deserialize<'de>>(s: &'de str) -> Result<T> {
    deser_json::from_str(s).map_err(|e| Error::Input(e.to_string()))
}

pub fn from_slice<'de, T: deser::Deserialize<'de>>(b: &'de [u8]) -> Result<T> {
    deser_json::from_slice(b).map_err(|e| Error::Input(e.to_string()))
}

pub fn to_string<T: deser::Serialize + ?Sized>(v: &T) -> Result<String> {
    deser_json::to_string(v).map_err(|e| Error::Operation(e.to_string()))
}

pub fn to_string_pretty<T: deser::Serialize + ?Sized>(v: &T) -> Result<String> {
    PRETTY.to_string(v).map_err(|e| Error::Operation(e.to_string()))
}

pub fn value_from_str(s: &str) -> Result<Value> {
    from_str(s)
}
