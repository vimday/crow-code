//! UTF-8 safe string truncation and token formatting utilities.
//!
//! Ported from yomi's `utils/strs.rs` and `utils/tokens.rs`.
//! Guarantees truncation never breaks multi-byte UTF-8 boundaries.

/// Truncate a string by byte length with a custom suffix (UTF-8 safe).
///
/// Finds a valid UTF-8 boundary before truncating.
///
/// # Behavior
/// - If `s.len() <= max_bytes`: returns `s` as-is (no suffix added)
/// - If `s.len() > max_bytes`: truncates to `max_bytes - suffix.len()` bytes
///   at a valid char boundary, then appends `suffix`
///
/// This ensures the result never exceeds `max_bytes` bytes (unless suffix
/// itself is longer).
pub fn truncate_with_suffix(s: &str, max_bytes: usize, suffix: &str) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }

    let target_len = max_bytes.saturating_sub(suffix.len());
    if target_len == 0 {
        return suffix.to_string();
    }

    let mut byte_idx = 0;
    for (idx, ch) in s.char_indices() {
        if idx + ch.len_utf8() > target_len {
            break;
        }
        byte_idx = idx + ch.len_utf8();
    }

    format!("{}{suffix}", &s[..byte_idx])
}

/// Truncate output with the standard "[Output truncated]" suffix.
pub fn truncate_tool_output(output: &str, max_bytes: usize) -> String {
    truncate_with_suffix(output, max_bytes, "\n\n[Output truncated due to limit]")
}

// ─── Token Estimation ───────────────────────────────────────────────

/// Estimate tokens from text length.
/// Rough approximation: 1 token ≈ 4 characters (bytes).
#[must_use]
pub const fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.len() / 4
}

/// Estimate tokens for JSON content.
/// JSON is denser (many single-char tokens like `{`, `}`, `:`, `,`).
/// Uses 2 chars/token instead of 4.
#[must_use]
pub const fn estimate_tokens_json(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.len() / 2
}

/// Format a token count for human display.
///
/// - `count < 1000` → `"~123"`
/// - `count >= 1000` → `"~1.5k"`
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_token_count(count: usize) -> String {
    if count >= 1000 {
        format!("~{:.1}k", count as f64 / 1000.0)
    } else {
        format!("~{count}")
    }
}

/// Format an actual (API-reported) token count for display (no `~` prefix).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_tokens(count: u32) -> String {
    if count >= 1000 {
        format!("{:.1}k", f64::from(count) / 1000.0)
    } else {
        count.to_string()
    }
}

/// Format elapsed time in a compact form.
///
/// - `< 60s` → `"12s"`
/// - `>= 60s` → `"1m 30s"`
#[must_use]
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {s}s")
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_needed() {
        assert_eq!(truncate_with_suffix("hello", 10, "..."), "hello");
        assert_eq!(truncate_with_suffix("hello", 5, "..."), "hello");
    }

    #[test]
    fn basic_truncation() {
        assert_eq!(truncate_with_suffix("hello world", 8, "..."), "hello...");
        assert_eq!(truncate_with_suffix("hello world", 5, "..."), "he...");
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_with_suffix("", 10, "..."), "");
        assert_eq!(truncate_with_suffix("", 0, "..."), "");
    }

    #[test]
    fn unicode_cjk() {
        let text = "你好世界"; // 12 bytes (4 × 3 bytes)
        assert_eq!(truncate_with_suffix(text, 12, "..."), "你好世界");
        assert_eq!(truncate_with_suffix(text, 6, "..."), "你...");
    }

    #[test]
    fn unicode_emoji() {
        let emoji = "🎉🎊🎁"; // 12 bytes (3 × 4 bytes)
        assert_eq!(truncate_with_suffix(emoji, 7, "..."), "🎉...");
    }

    #[test]
    fn mixed_unicode() {
        let text = "Hello你好World世界";
        let result = truncate_with_suffix(text, 10, "...");
        assert!(
            result.len() <= 10,
            "Result too long: {} bytes",
            result.len()
        );
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn suffix_larger_than_limit() {
        assert_eq!(truncate_with_suffix("hello", 2, "..."), "...");
        assert_eq!(truncate_with_suffix("hello", 0, "..."), "...");
    }

    #[test]
    fn never_breaks_char_boundary() {
        let cases = [
            ("hello world", 8, "..."),
            ("你好世界", 6, "..."),
            ("🎉🎊🎁", 7, "..."),
            ("αβγδ", 5, "..."),
        ];
        for (text, max, suffix) in cases {
            let result = truncate_with_suffix(text, max, suffix);
            assert!(
                std::str::from_utf8(result.as_bytes()).is_ok(),
                "Invalid UTF-8 for input '{text}'"
            );
            assert!(
                result.len() <= max || result == suffix,
                "Result '{result}' ({} bytes) exceeds max {max}",
                result.len()
            );
        }
    }

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("hello world"), 2);
    }

    #[test]
    fn estimate_tokens_json_denser() {
        let json = r#"{"key": "value"}"#;
        assert!(estimate_tokens_json(json) > estimate_tokens(json));
    }

    #[test]
    fn format_token_count_display() {
        assert_eq!(format_token_count(100), "~100");
        assert_eq!(format_token_count(1500), "~1.5k");
        assert_eq!(format_token_count(10000), "~10.0k");
    }

    #[test]
    fn format_tokens_actual() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5k");
    }

    #[test]
    fn format_elapsed_display() {
        assert_eq!(format_elapsed(12), "12s");
        assert_eq!(format_elapsed(60), "1m");
        assert_eq!(format_elapsed(90), "1m 30s");
        assert_eq!(format_elapsed(0), "0s");
    }
}
