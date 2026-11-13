//! Segment-wise glob matching for keep lists.
//! Supports literal segments, `*` and `?` within a segment, and `**` for
//! zero or more whole segments. Patterns and paths are slash-separated and
//! match the whole path.

pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = split(pattern);
    let path = split(path);
    match_segments(&pattern, &path)
}

fn split(value: &str) -> Vec<&str> {
    value.split('/').filter(|part| !part.is_empty()).collect()
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        for start in 0..=path.len() {
            if match_segments(&pattern[1..], &path[start..]) {
                return true;
            }
        }
        return false;
    }
    if path.is_empty() {
        return false;
    }
    match_one(pattern[0], path[0]) && match_segments(&pattern[1..], &path[1..])
}

fn match_one(pattern: &str, segment: &str) -> bool {
    let pattern = pattern.as_bytes();
    let segment = segment.as_bytes();
    let mut p = 0;
    let mut s = 0;
    let mut star = usize::MAX;
    let mut star_segment = 0;
    while s < segment.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == segment[s]) {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = p;
            star_segment = s;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            star_segment += 1;
            s = star_segment;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn literal_segments_match_exactly() {
        assert!(glob_match("bin/node", "bin/node"));
        assert!(!glob_match("bin/node", "bin/npm"));
        assert!(!glob_match("bin/node", "bin/node/x"));
        assert!(!glob_match("a/b", "a"));
    }

    #[test]
    fn star_matches_within_one_segment() {
        assert!(glob_match("bin/n*", "bin/node"));
        assert!(glob_match("bin/n?de", "bin/node"));
        assert!(!glob_match("bin/n*", "bin/lib/node"));
        assert!(!glob_match("b*/node", "bin/lib/node"));
    }

    #[test]
    fn double_star_spans_segments() {
        assert!(glob_match("bin/**", "bin/node"));
        assert!(glob_match("bin/**", "bin/a/b/c"));
        assert!(!glob_match("**/npm-cli.js", "bin/npm"));
        assert!(glob_match("**/npm-cli.js", "lib/node_modules/npm/bin/npm-cli.js"));
        assert!(glob_match("**", "a/b/c"));
        assert!(glob_match("lib/**/bin/*", "lib/node_modules/npm/bin/npm-cli.js"));
        assert!(glob_match("bin/**", "bin"));
    }

    #[test]
    fn trailing_double_star_matches_the_directory_itself() {
        assert!(glob_match("bin/**", "bin"));
    }

    #[test]
    fn empties_and_slashes_are_normalized() {
        assert!(glob_match("/bin/node/", "bin/node"));
        assert!(glob_match("bin//node", "bin/node"));
    }
}
