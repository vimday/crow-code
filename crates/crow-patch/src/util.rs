//! Shared utilities for the crow workspace.

/// UTF-8 safe truncation — never panics on multibyte character boundaries.
///
/// Returns a string slice of at most `max_bytes` bytes, backing off to the
/// nearest valid char boundary when the limit falls mid-codepoint.
pub fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Compute a hex-encoded SHA-256 digest of the given bytes.
///
/// This is the **single source of truth** for content hashing across all
/// crow crates. Both the hydrator (precondition injection) and the applier
/// (precondition verification) must use this function to guarantee identical
/// outputs.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    hex::encode(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_within_limit() {
        assert_eq!(safe_truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_exact_limit() {
        assert_eq!(safe_truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_ascii() {
        assert_eq!(safe_truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_multibyte_boundary() {
        // '日' is 3 bytes in UTF-8
        let s = "日本語";
        assert_eq!(safe_truncate(s, 3), "日");
        assert_eq!(safe_truncate(s, 4), "日"); // mid-codepoint: backs off
        assert_eq!(safe_truncate(s, 6), "日本");
    }

    #[test]
    fn truncate_empty() {
        assert_eq!(safe_truncate("", 10), "");
        assert_eq!(safe_truncate("hello", 0), "");
    }
}
