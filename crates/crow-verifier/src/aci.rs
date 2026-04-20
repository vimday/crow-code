//! Adaptive Context Integration (ACI) log truncation.
//!
//! Raw command output can be megabytes of test output, compiler
//! diagnostics, or build logs. The LLM's context window is finite
//! and expensive. ACI keeps the most diagnostic-value lines:
//!
//! - **Head**: first N lines (compilation errors, test discovery, headers)
//! - **Tail**: last M lines (test summary, exit status, final errors)
//! - **Omission marker**: shows how many lines were dropped
//!
//! This is a pure function with no side effects — it takes a string
//! and returns a truncated string.

use crate::types::AciConfig;

/// The omission marker inserted between head and tail.
const OMISSION_MARKER: &str = "\n... [crow-aci] {} lines omitted ...\n";

/// Truncate raw output according to ACI configuration.
///
/// If the output fits within `max_lines`, it is returned unchanged.
/// Otherwise, keep `head_lines` from the top and `tail_lines` from
/// the bottom, with an omission marker in between.
pub fn truncate(raw: &str, config: &AciConfig) -> AciResult {
    let mut head = Vec::with_capacity(config.head_lines);
    let mut tail = std::collections::VecDeque::with_capacity(config.tail_lines);
    let mut total = 0;

    for line in raw.lines() {
        total += 1;
        if head.len() < config.head_lines {
            head.push(line);
        } else {
            if tail.len() == config.tail_lines && config.tail_lines > 0 {
                tail.pop_front();
            }
            if config.tail_lines > 0 {
                tail.push_back(line);
            }
        }
    }

    if total <= config.max_lines || config.head_lines + config.tail_lines >= total {
        return AciResult {
            output: raw.to_string(),
            original_lines: total,
            retained_lines: total,
            omitted_lines: 0,
            was_truncated: false,
        };
    }

    let omitted = total - config.head_lines - config.tail_lines;
    let marker = OMISSION_MARKER.replace("{}", &omitted.to_string());

    let mut result = String::with_capacity(
        head.iter().map(|s| s.len() + 1).sum::<usize>()
            + marker.len()
            + tail.iter().map(|s| s.len() + 1).sum::<usize>(),
    );

    for line in head {
        result.push_str(line);
        result.push('\n');
    }

    result.push_str(&marker);

    for (i, line) in tail.iter().enumerate() {
        result.push_str(line);
        if i < tail.len() - 1 {
            result.push('\n');
        }
    }

    AciResult {
        output: result,
        original_lines: total,
        retained_lines: config.head_lines + config.tail_lines,
        omitted_lines: omitted,
        was_truncated: true,
    }
}

/// The result of ACI truncation, with metadata for observability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AciResult {
    /// The truncated (or original) output string.
    pub output: String,
    /// Total lines in the original output.
    pub original_lines: usize,
    /// Lines retained after truncation.
    pub retained_lines: usize,
    /// Lines omitted by truncation.
    pub omitted_lines: usize,
    /// Whether any truncation occurred.
    pub was_truncated: bool,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered_output(n: usize) -> String {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn short_output_is_not_truncated() {
        let output = numbered_output(10);
        let config = AciConfig::default_config(); // 200 lines

        let result = truncate(&output, &config);

        assert!(!result.was_truncated);
        assert_eq!(result.original_lines, 10);
        assert_eq!(result.retained_lines, 10);
        assert_eq!(result.omitted_lines, 0);
        assert_eq!(result.output, output);
    }

    #[test]
    fn exact_max_lines_is_not_truncated() {
        let config = AciConfig {
            max_lines: 20,
            head_lines: 5,
            tail_lines: 15,
        };
        let output = numbered_output(20);

        let result = truncate(&output, &config);

        assert!(!result.was_truncated);
        assert_eq!(result.retained_lines, 20);
    }

    #[test]
    fn truncation_keeps_head_and_tail() {
        let config = AciConfig {
            max_lines: 10,
            head_lines: 3,
            tail_lines: 7,
        };
        let output = numbered_output(100);

        let result = truncate(&output, &config);

        assert!(result.was_truncated);
        assert_eq!(result.original_lines, 100);
        assert_eq!(result.retained_lines, 10); // 3 + 7
        assert_eq!(result.omitted_lines, 90); // 100 - 3 - 7

        // Verify head content
        assert!(result.output.starts_with("line 1\n"));
        assert!(result.output.contains("line 3\n"));

        // Verify omission marker
        assert!(result.output.contains("[crow-aci] 90 lines omitted"));

        // Verify tail content
        assert!(result.output.contains("line 94\n"));
        assert!(result.output.ends_with("line 100"));
    }

    #[test]
    fn compact_config_truncation() {
        let config = AciConfig::compact(); // 80 total, 20 head, 60 tail
        let output = numbered_output(500);

        let result = truncate(&output, &config);

        assert!(result.was_truncated);
        assert_eq!(result.retained_lines, 80);
        assert_eq!(result.omitted_lines, 420);
        assert!(result.output.contains("[crow-aci] 420 lines omitted"));
    }

    #[test]
    fn single_line_output() {
        let output = "ok";
        let config = AciConfig::default_config();

        let result = truncate(output, &config);

        assert!(!result.was_truncated);
        assert_eq!(result.output, "ok");
    }

    #[test]
    fn empty_output() {
        let output = "";
        let config = AciConfig::default_config();

        let result = truncate(output, &config);

        assert!(!result.was_truncated);
        assert_eq!(result.original_lines, 0);
    }

    #[test]
    fn head_preserves_compiler_errors() {
        let mut lines = vec![
            "error[E0308]: mismatched types",
            "  --> src/main.rs:42:5",
            "   |",
            "42 |     let x: u32 = \"hello\";",
            "   |                  ^^^^^^^ expected u32",
        ];
        // Pad with 200 more lines of noise
        let noise: Vec<String> = (0..200).map(|i| format!("test {i} ... ok")).collect();
        let noise_refs: Vec<&str> = noise.iter().map(std::string::String::as_str).collect();
        lines.extend_from_slice(&noise_refs);
        // Add a summary tail
        lines.push("test result: FAILED. 5 passed; 1 failed");

        let output = lines.join("\n");
        let config = AciConfig {
            max_lines: 10,
            head_lines: 5,
            tail_lines: 5,
        };

        let result = truncate(&output, &config);

        assert!(result.was_truncated);
        // Head should contain the error
        assert!(result.output.contains("error[E0308]: mismatched types"));
        assert!(result.output.contains("expected u32"));
        // Tail should contain the summary
        assert!(result.output.contains("test result: FAILED"));
    }

    #[test]
    fn omission_marker_format() {
        let config = AciConfig {
            max_lines: 4,
            head_lines: 2,
            tail_lines: 2,
        };
        let output = numbered_output(10);

        let result = truncate(&output, &config);

        // Should contain exactly one marker
        let marker_count = result.output.matches("[crow-aci]").count();
        assert_eq!(marker_count, 1);
        assert!(result.output.contains("6 lines omitted"));
    }
}
