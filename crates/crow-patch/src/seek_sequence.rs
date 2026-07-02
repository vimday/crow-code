//! 4-tier fuzzy sequence matching for patch application.
//!
//! Tries progressively looser matching strategies to locate a needle
//! sequence within a haystack of file lines.

// ─── Types ──────────────────────────────────────────────────────────

/// The tier of match that was used to locate a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchTier {
    /// Byte-for-byte equality.
    Exact,
    /// Equal after trimming trailing whitespace.
    TrimEnd,
    /// Equal after trimming both leading and trailing whitespace.
    TrimBoth,
    /// Equal after Unicode normalization and trimming.
    UnicodeNormalize,
}

/// Result of a successful sequence seek.
#[derive(Debug, Clone)]
pub struct SeekResult {
    /// 0-based index into the haystack where the match begins.
    pub start_line: usize,
    /// Which matching tier was used.
    pub match_tier: MatchTier,
}

// ─── Public API ─────────────────────────────────────────────────────

/// Search `haystack[start_from..]` for a contiguous run of lines
/// matching every line in `needle`, trying increasingly fuzzy tiers.
///
/// Returns [`None`] if no tier produces a match.
pub fn seek_sequence(
    haystack: &[String],
    needle: &[String],
    start_from: usize,
) -> Option<SeekResult> {
    if needle.is_empty() {
        return Some(SeekResult {
            start_line: start_from,
            match_tier: MatchTier::Exact,
        });
    }

    type Comparator = fn(&str, &str) -> bool;
    let tiers: &[(MatchTier, Comparator)] = &[
        (MatchTier::Exact, cmp_exact),
        (MatchTier::TrimEnd, cmp_trim_end),
        (MatchTier::TrimBoth, cmp_trim_both),
        (MatchTier::UnicodeNormalize, cmp_unicode_normalize),
    ];

    for &(tier, comparator) in tiers {
        if let Some(pos) = scan_for_match(haystack, needle, start_from, comparator) {
            return Some(SeekResult {
                start_line: pos,
                match_tier: tier,
            });
        }
    }

    None
}

// ─── Scanning ───────────────────────────────────────────────────────

/// Scan `haystack[start_from..]` for a contiguous match of `needle`
/// using the given comparator.
fn scan_for_match(
    haystack: &[String],
    needle: &[String],
    start_from: usize,
    comparator: fn(&str, &str) -> bool,
) -> Option<usize> {
    if needle.len() > haystack.len().saturating_sub(start_from) {
        return None;
    }
    let end = haystack.len() - needle.len() + 1;
    (start_from..end).find(|&pos| try_match_at_tier(haystack, needle, pos, comparator))
}

/// Check whether every line in `needle` matches the corresponding line
/// in `haystack` starting at `pos`.
fn try_match_at_tier(
    haystack: &[String],
    needle: &[String],
    pos: usize,
    comparator: fn(&str, &str) -> bool,
) -> bool {
    needle
        .iter()
        .enumerate()
        .all(|(i, n)| comparator(&haystack[pos + i], n))
}

// ─── Comparators ────────────────────────────────────────────────────

fn cmp_exact(a: &str, b: &str) -> bool {
    a == b
}

fn cmp_trim_end(a: &str, b: &str) -> bool {
    a.trim_end() == b.trim_end()
}

fn cmp_trim_both(a: &str, b: &str) -> bool {
    a.trim() == b.trim()
}

fn cmp_unicode_normalize(a: &str, b: &str) -> bool {
    normalize_unicode(a).trim() == normalize_unicode(b).trim()
}

// ─── Unicode normalization ──────────────────────────────────────────

/// Replace common Unicode typographic characters with their ASCII
/// equivalents for fuzzy matching.
fn normalize_unicode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201C}' | '\u{201D}' => out.push('"'),
            '\u{2013}' | '\u{2014}' => out.push('-'),
            '\u{00A0}' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn exact_match() {
        let hay = s(&["a", "b", "c", "d"]);
        let needle = s(&["b", "c"]);
        let res = seek_sequence(&hay, &needle, 0).unwrap();
        assert_eq!(res.start_line, 1);
        assert_eq!(res.match_tier, MatchTier::Exact);
    }

    #[test]
    fn trim_end_match() {
        let hay = s(&["a", "b  ", "c\t", "d"]);
        let needle = s(&["b", "c"]);
        let res = seek_sequence(&hay, &needle, 0).unwrap();
        assert_eq!(res.start_line, 1);
        assert_eq!(res.match_tier, MatchTier::TrimEnd);
    }

    #[test]
    fn trim_both_match() {
        let hay = s(&["a", "  b", "  c  ", "d"]);
        let needle = s(&["b", "c"]);
        let res = seek_sequence(&hay, &needle, 0).unwrap();
        assert_eq!(res.start_line, 1);
        assert_eq!(res.match_tier, MatchTier::TrimBoth);
    }

    #[test]
    fn unicode_normalize_match() {
        let hay = s(&["a", "it\u{2019}s here", "d"]);
        let needle = s(&["it's here"]);
        let res = seek_sequence(&hay, &needle, 0).unwrap();
        assert_eq!(res.start_line, 1);
        assert_eq!(res.match_tier, MatchTier::UnicodeNormalize);
    }

    #[test]
    fn no_match() {
        let hay = s(&["a", "b", "c"]);
        let needle = s(&["x", "y"]);
        assert!(seek_sequence(&hay, &needle, 0).is_none());
    }

    #[test]
    fn empty_needle() {
        let hay = s(&["a", "b"]);
        let res = seek_sequence(&hay, &[], 0).unwrap();
        assert_eq!(res.start_line, 0);
        assert_eq!(res.match_tier, MatchTier::Exact);
    }

    #[test]
    fn start_from_offset() {
        let hay = s(&["a", "b", "a", "b"]);
        let needle = s(&["a", "b"]);
        let res = seek_sequence(&hay, &needle, 1).unwrap();
        assert_eq!(res.start_line, 2);
    }
}
