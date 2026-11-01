const MAX_GENERATION_ID_LEN: usize = 128;

pub fn parse_generation_id(text: &str) -> Option<&str> {
    if text.is_empty() || text.len() > MAX_GENERATION_ID_LEN {
        return None;
    }
    if !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_ids_are_bounded_path_safe_names() {
        let max = "a".repeat(MAX_GENERATION_ID_LEN);
        for id in ["gen1", "g", "demo-v1", "a_b-c9", max.as_str()] {
            assert_eq!(parse_generation_id(id), Some(id));
        }
        for id in ["", ".", "..", "gen/1", "gen.1", "gen 1", "gén", &"a".repeat(MAX_GENERATION_ID_LEN + 1)] {
            assert_eq!(parse_generation_id(id), None, "{id:?}");
        }
    }
}
