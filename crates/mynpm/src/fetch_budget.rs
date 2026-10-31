#![forbid(unsafe_code)]

use crate::error::{Error, Result};

pub struct FetchByteBudget {
    remaining: u64,
}

impl FetchByteBudget {
    pub fn new() -> Self {
        Self { remaining: crate::limits::FETCH_BYTES }
    }

    pub fn take(&mut self, bytes: u64) -> Result<()> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or_else(|| {
            Error::Limit(format!(
                "fetch byte budget exceeded (limit {})",
                crate::limits::format_bytes(crate::limits::FETCH_BYTES)
            ))
        })?;
        Ok(())
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

impl Default for FetchByteBudget {
    fn default() -> Self {
        Self::new()
    }
}

pub fn create_fetch_byte_budget() -> FetchByteBudget {
    FetchByteBudget::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_depletes_and_errors() {
        let mut budget = create_fetch_byte_budget();
        assert_eq!(budget.remaining(), crate::limits::FETCH_BYTES);
        budget.take(10).unwrap();
        assert_eq!(budget.remaining(), crate::limits::FETCH_BYTES - 10);
        assert!(matches!(budget.take(u64::MAX), Err(Error::Limit(_))));
    }
}
