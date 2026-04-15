//! Fast compile-check preflight.
//!
//! Runs `cargo check --message-format=json` inside a sandbox before the
//! full test suite. If the patched code has compile errors, this catches
//! them in 2-5 seconds instead of 30-60 seconds, preserving crucible
//! attempts for real semantic failures.

use std::path::Path;
use std::time::Duration;

// ─── Diagnostic ─────────────────────────────────────────────────────

/// A single compile-time diagnostic extracted from `cargo check` output.
#[derive(Debug, Clone)]
pub struct CompileDiagnostic {
    pub level: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

impl std::fmt::Display for CompileDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let (Some(file), Some(line)) = (&self.file, self.line) {
            write!(f, "{}:{}: {}: {}", file, line, self.level, self.message)
        } else {
            write!(f, "{}: {}", self.level, self.message)
        }
    }
}

// ─── Preflight ──────────────────────────────────────────────────────

/// Result of a preflight check.
#[derive(Debug)]
pub enum PreflightResult {
    /// Code compiles cleanly — proceed to full test suite.
    Clean,
    /// Compile errors found — return diagnostics to the LLM.
    Errors(Vec<CompileDiagnostic>),
    /// Preflight itself failed (e.g. cargo not found, timeout).
    /// Not a code error — fall through to full test suite.
    Skipped(String),
}

/// Run `cargo check --message-format=json` as a fast pre-flight.
///
/// Returns `PreflightResult::Clean` if compilation succeeds,
/// or `PreflightResult::Errors` with structured diagnostics if it fails.
/// Never blocks the caller for more than `timeout`.
pub async fn cargo_check_preflight(
    sandbox_root: &Path,
    cache_root: Option<&Path>,
    timeout: Duration,
) -> PreflightResult {
    use crate::executor;
    use crate::types::{AciConfig, ExecutionConfig};
    use crow_probe::VerificationCommand;

    let cmd = VerificationCommand::new(
        "cargo",
        vec!["check", "--message-format=json", "--color=never"],
    );

    let exec_config = ExecutionConfig {
        timeout,
        max_output_bytes: 2 * 1024 * 1024, // 2 MB should be plenty for check output
    };
    let aci_config = AciConfig::default_config();

    let result =
        match executor::execute(sandbox_root, &cmd, &exec_config, &aci_config, cache_root).await {
            Ok(r) => r,
            Err(e) => {
                return PreflightResult::Skipped(format!("cargo check failed to execute: {}", e));
            }
        };

    // Exit code 0 → compilation succeeded
    if result.exit_code == Some(0) {
        return PreflightResult::Clean;
    }

    // Parse JSON diagnostics from the output
    let diagnostics = parse_cargo_diagnostics(&result.test_run.truncated_log);

    if diagnostics.is_empty() {
        // Compilation failed but we couldn't parse diagnostics
        // (e.g. timeout or non-JSON output). Fall through to full test.
        return PreflightResult::Skipped(format!(
            "cargo check exit={:?} but no parseable diagnostics",
            result.exit_code
        ));
    }

    PreflightResult::Errors(diagnostics)
}

/// Format a list of compile diagnostics into a concise string for the LLM.
///
/// Caps output at 10 errors to avoid flooding the context window.
pub fn format_diagnostics(diags: &[CompileDiagnostic]) -> String {
    const MAX_SHOWN: usize = 10;
    let mut out = String::from("[COMPILE ERRORS — fix these before resubmitting]\n\n");
    for (i, d) in diags.iter().take(MAX_SHOWN).enumerate() {
        out.push_str(&format!("  {}. {}\n", i + 1, d));
    }
    if diags.len() > MAX_SHOWN {
        out.push_str(&format!(
            "\n  ... and {} more error(s) not shown\n",
            diags.len() - MAX_SHOWN
        ));
    }
    out
}

// ─── JSON Parser ────────────────────────────────────────────────────

/// Parse `cargo check --message-format=json` output into diagnostics.
///
/// Each line of output is a JSON object. We look for objects with
/// `reason: "compiler-message"` and extract the nested `message` fields.
fn parse_cargo_diagnostics(output: &str) -> Vec<CompileDiagnostic> {
    let mut diagnostics = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse each line as JSON
        let data: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only process compiler-message entries
        if data["reason"].as_str() != Some("compiler-message") {
            continue;
        }

        let msg = &data["message"];
        let level = msg["level"].as_str().unwrap_or("unknown");

        // Only report errors (skip warnings for speed)
        if level != "error" {
            continue;
        }

        let message = msg["message"]
            .as_str()
            .unwrap_or("unknown error")
            .to_string();

        // Extract the primary span location
        let (file, line_num, column) = if let Some(spans) = msg["spans"].as_array() {
            if let Some(primary) = spans
                .iter()
                .find(|s| s["is_primary"].as_bool() == Some(true))
            {
                (
                    primary["file_name"].as_str().map(String::from),
                    primary["line_start"].as_u64().map(|n| n as usize),
                    primary["column_start"].as_u64().map(|n| n as usize),
                )
            } else {
                (None, None, None)
            }
        } else {
            (None, None, None)
        };

        diagnostics.push(CompileDiagnostic {
            level: level.to_string(),
            message,
            file,
            line: line_num,
            column,
        });
    }

    diagnostics
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cargo_json_errors() {
        let output = r#"{"reason":"compiler-artifact","package_id":"foo","target":{"name":"foo"}}
{"reason":"compiler-message","package_id":"foo","message":{"message":"expected `;`","code":null,"level":"error","spans":[{"file_name":"src/main.rs","byte_start":45,"byte_end":45,"line_start":3,"line_end":3,"column_start":22,"column_end":22,"is_primary":true,"text":[]}],"children":[],"rendered":"error: expected `;`"}}
{"reason":"compiler-message","package_id":"foo","message":{"message":"unused variable: `x`","code":{"code":"unused_variables"},"level":"warning","spans":[{"file_name":"src/main.rs","byte_start":10,"byte_end":11,"line_start":1,"line_end":1,"column_start":5,"column_end":6,"is_primary":true,"text":[]}],"children":[],"rendered":"warning: unused variable"}}
{"reason":"build-finished","success":false}"#;

        let diags = parse_cargo_diagnostics(output);

        // Should only pick up errors, not warnings
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].level, "error");
        assert_eq!(diags[0].message, "expected `;`");
        assert_eq!(diags[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(diags[0].line, Some(3));
        assert_eq!(diags[0].column, Some(22));
    }

    #[test]
    fn parse_empty_output() {
        let diags = parse_cargo_diagnostics("");
        assert!(diags.is_empty());
    }

    #[test]
    fn parse_non_json_lines_skipped() {
        let output = "Compiling foo v0.1.0\nerror[E0308]: mismatched types\nnot json at all";
        let diags = parse_cargo_diagnostics(output);
        assert!(diags.is_empty());
    }

    #[test]
    fn format_diagnostics_output() {
        let diags = vec![CompileDiagnostic {
            level: "error".into(),
            message: "expected `;`".into(),
            file: Some("src/main.rs".into()),
            line: Some(3),
            column: Some(22),
        }];
        let formatted = format_diagnostics(&diags);
        assert!(formatted.contains("COMPILE ERRORS"));
        assert!(formatted.contains("src/main.rs:3: error: expected `;`"));
    }

    #[test]
    fn format_diagnostics_caps_at_ten() {
        let diags: Vec<CompileDiagnostic> = (0..15)
            .map(|i| CompileDiagnostic {
                level: "error".into(),
                message: format!("error number {}", i),
                file: Some("src/lib.rs".into()),
                line: Some(i + 1),
                column: None,
            })
            .collect();
        let formatted = format_diagnostics(&diags);
        // Should show errors 1-10
        assert!(formatted.contains("1. src/lib.rs:1: error: error number 0"));
        assert!(formatted.contains("10. src/lib.rs:10: error: error number 9"));
        // Should NOT show error 11
        assert!(!formatted.contains("11."));
        // Should show remainder
        assert!(formatted.contains("5 more error(s) not shown"));
    }
}
