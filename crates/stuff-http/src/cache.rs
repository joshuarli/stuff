#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    ImmutableFile,
    GenerationPointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    pub max_age_secs: u64,
    pub immutable: bool,
}

impl CachePolicy {
    pub fn header_value(&self) -> String {
        if self.immutable {
            format!("public, max-age={}, immutable", self.max_age_secs)
        } else {
            format!("public, max-age={}", self.max_age_secs)
        }
    }
}

pub fn cache_policy_for(kind: RouteKind) -> CachePolicy {
    match kind {
        RouteKind::ImmutableFile => CachePolicy {
            max_age_secs: 31_536_000,
            immutable: true,
        },
        RouteKind::GenerationPointer => CachePolicy {
            max_age_secs: 60,
            immutable: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies() {
        let imm = cache_policy_for(RouteKind::ImmutableFile);
        assert_eq!(imm.header_value(), "public, max-age=31536000, immutable");
        let ptr = cache_policy_for(RouteKind::GenerationPointer);
        assert_eq!(ptr.header_value(), "public, max-age=60");
    }
}
