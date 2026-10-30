#![forbid(unsafe_code)]

use crate::error::{Error, Result};

pub type Value = deser_value::Value;

pub fn from_str<'de, T: deser::Deserialize<'de>>(s: &'de str) -> Result<T> {
    deser_toml::from_str(s).map_err(|e| Error::Input(e.to_string()))
}

pub fn from_slice<'de, T: deser::Deserialize<'de>>(b: &'de [u8]) -> Result<T> {
    deser_toml::from_slice(b).map_err(|e| Error::Input(e.to_string()))
}

pub fn to_string<T: deser::Serialize + ?Sized>(v: &T) -> Result<String> {
    deser_toml::to_string(v).map_err(|e| Error::Operation(e.to_string()))
}

pub fn value_from_str(s: &str) -> Result<Value> {
    from_str(s)
}
