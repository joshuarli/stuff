#![forbid(unsafe_code)]

pub fn shard_for_name(name_lower: &str) -> Option<String> {
    if !name_lower.is_ascii() {
        return None;
    }
    if name_lower.is_empty() {
        return Some("1".to_string());
    }
    if name_lower.len() == 1 {
        return Some(format!("1/{}", name_lower));
    }
    if name_lower.len() == 2 {
        return Some(format!("2/{}", name_lower));
    }
    if name_lower.len() == 3 {
        return Some(format!("3/{}/{}", &name_lower[0..1], name_lower));
    }
    Some(format!("{}/{}/{}", &name_lower[0..2], &name_lower[2..4], name_lower))
}

#[cfg(test)]
mod tests {
    use super::shard_for_name;

    #[test]
    fn shards_like_the_crates_io_sparse_index() {
        assert_eq!(shard_for_name(""), Some("1".to_string()));
        assert_eq!(shard_for_name("a"), Some("1/a".to_string()));
        assert_eq!(shard_for_name("ab"), Some("2/ab".to_string()));
        assert_eq!(shard_for_name("abc"), Some("3/a/abc".to_string()));
        assert_eq!(shard_for_name("abcd"), Some("ab/cd/abcd".to_string()));
        assert_eq!(shard_for_name("serde_json"), Some("se/rd/serde_json".to_string()));
    }

    #[test]
    fn rejects_non_ascii_names_instead_of_slicing_them() {
        assert_eq!(shard_for_name("café"), None);
        assert_eq!(shard_for_name("日本語"), None);
    }
}
