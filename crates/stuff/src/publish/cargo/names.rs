use super::{input, Rejection};

pub const MAX_NAME_LENGTH: usize = 64;

pub fn validate_crate_name(name: &str) -> Result<(), Rejection> {
    if name.is_empty() {
        return Err(input("empty crate name"));
    }
    let Some(first) = name.chars().next() else {
        return Err(input("empty crate name"));
    };
    if !first.is_ascii_alphabetic() {
        return Err(input("crate name must start with letter"));
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(input("crate name too long"));
    }
    for character in name.chars() {
        if !(character.is_ascii_alphanumeric() || character == '-' || character == '_') {
            return Err(input("invalid crate name character"));
        }
    }
    Ok(())
}

pub fn lower_name(name: &str) -> String {
    name.to_lowercase()
}

pub fn normalized_key(name: &str) -> String {
    name.to_lowercase().replace('_', "-")
}

pub fn shard_for_name(name_lower: &str) -> Option<String> {
    stuff_cargo_shard::shard_for_name(name_lower)
}
