#![forbid(unsafe_code)]

pub fn shard_for_name(name_lower: &str) -> String {
    let bytes = name_lower.as_bytes();
    if bytes.is_empty() {
        return "1".to_string();
    }
    if name_lower.len() == 1 {
        return format!("1/{}", name_lower);
    }
    if name_lower.len() == 2 {
        return format!("2/{}", name_lower);
    }
    if name_lower.len() == 3 {
        return format!("3/{}/{}", &name_lower[0..1], name_lower);
    }
    format!("{}/{}/{}", &name_lower[0..2], &name_lower[2..4], name_lower)
}

#[cfg(test)]
mod tests {
    use super::shard_for_name;

    #[test]
    fn shards_like_the_crates_io_sparse_index() {
        assert_eq!(shard_for_name(""), "1");
        assert_eq!(shard_for_name("a"), "1/a");
        assert_eq!(shard_for_name("ab"), "2/ab");
        assert_eq!(shard_for_name("abc"), "3/a/abc");
        assert_eq!(shard_for_name("abcd"), "ab/cd/abcd");
        assert_eq!(shard_for_name("serde_json"), "se/rd/serde_json");
    }
}
