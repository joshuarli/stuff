#![forbid(unsafe_code)]

use crate::digest::Digest;
use rustc_hash::FxHashMap;

pub const GENERATION_SCHEMA: &[u8] = b"stuff-oci-generation-v1\0";
pub const GENERATION_ID_LEN: usize = 24;

pub fn generation_id(files: &FxHashMap<String, Vec<u8>>) -> String {
    let mut payload = GENERATION_SCHEMA.to_vec();
    let mut entries: Vec<_> = files.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (relative, bytes) in entries {
        payload.extend_from_slice(relative.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        payload.extend_from_slice(bytes);
    }
    Digest::from_bytes(&payload).hex()[..GENERATION_ID_LEN].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_ids_are_content_addressed() {
        let mut files = FxHashMap::default();
        files.insert("a".to_string(), b"one".to_vec());
        let first = generation_id(&files);
        assert_eq!(first.len(), GENERATION_ID_LEN);
        assert_eq!(generation_id(&files), first);
        files.insert("a".to_string(), b"two".to_vec());
        assert_ne!(generation_id(&files), first);
        files.insert("b".to_string(), b"one".to_vec());
        assert_ne!(generation_id(&files), first);
    }
}
