//! Structured environment context (Codex `environment_context.rs` pattern).
//!
//! Provides the agent with rich situational awareness about its execution
//! environment: working directory, shell, OS platform, date/timezone.
//! Rendered as XML-tagged context fragments injected into the system prompt.
//!
//! # Architecture
//!
//! ```text
//! <environment_context>
//!   <cwd>/path/to/workspace</cwd>
//!   <shell>zsh</shell>
//!   <platform>macos-arm64</platform>
//!   <current_date>2026-05-19</current_date>
//!   <timezone>UTC+8</timezone>
//! </environment_context>
//! ```

use std::fmt::Write;
use std::path::Path;

/// Rich environment context for the agent's situational awareness.
///
/// Mirrors Codex's `EnvironmentContext` — provides structured context
/// about the execution environment so the agent can make platform-aware
/// decisions (e.g., use `pbcopy` on macOS, `xclip` on Linux).
#[derive(Debug, Clone)]
pub struct CrowEnvironmentContext {
    /// Current working directory / workspace root.
    pub cwd: String,
    /// Shell name (e.g., "zsh", "bash", "fish").
    pub shell: String,
    /// Platform identifier (e.g., "macos-arm64", "linux-x86_64").
    pub platform: String,
    /// ISO 8601 date string.
    pub current_date: String,
    /// Timezone identifier.
    pub timezone: String,
}

impl CrowEnvironmentContext {
    /// Auto-detect environment context from the workspace root.
    ///
    /// Probes the host system for OS, shell, and date information.
    pub fn from_workspace(workspace_root: &Path) -> Self {
        Self {
            cwd: workspace_root.to_string_lossy().to_string(),
            shell: detect_shell(),
            platform: detect_platform(),
            current_date: current_date_iso(),
            timezone: detect_timezone(),
        }
    }

    /// Render as XML-tagged context block for injection into the system prompt.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("<environment_context>\n");
        let _ = writeln!(out, "  <cwd>{}</cwd>", self.cwd);
        let _ = writeln!(out, "  <shell>{}</shell>", self.shell);
        let _ = writeln!(out, "  <platform>{}</platform>", self.platform);
        let _ = writeln!(out, "  <current_date>{}</current_date>", self.current_date);
        let _ = writeln!(out, "  <timezone>{}</timezone>", self.timezone);
        out.push_str("</environment_context>");
        out
    }
}

/// Detect the user's default shell from the environment.
fn detect_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(String::from))
        .unwrap_or_else(|| "sh".to_string())
}

/// Detect the OS + architecture string.
fn detect_platform() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    format!("{os}-{arch}")
}

/// Get current date in ISO 8601 format (YYYY-MM-DD).
fn current_date_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple UTC date calculation (avoids chrono dependency)
    let days = secs / 86400;
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Detect the system timezone from the `TZ` environment variable,
/// falling back to a UTC offset estimate.
fn detect_timezone() -> String {
    std::env::var("TZ").unwrap_or_else(|_| "UTC".to_string())
}

/// Convert days since Unix epoch to (year, month, day).
/// Civil date algorithm from Howard Hinnant.
fn days_to_date(days: u64) -> (u32, u32, u32) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64 + era * 400) as u32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_environment_context_render() {
        let ctx = CrowEnvironmentContext::from_workspace(&PathBuf::from("/tmp/test"));
        let rendered = ctx.render();
        assert!(rendered.contains("<environment_context>"));
        assert!(rendered.contains("</environment_context>"));
        assert!(rendered.contains("<cwd>/tmp/test</cwd>"));
        assert!(rendered.contains("<platform>"));
    }

    #[test]
    fn test_platform_detection() {
        let platform = detect_platform();
        assert!(!platform.is_empty());
        // Should contain a dash separating OS and arch
        assert!(platform.contains('-'));
    }

    #[test]
    fn test_date_iso() {
        let date = current_date_iso();
        // Should be YYYY-MM-DD format
        assert_eq!(date.len(), 10);
        assert_eq!(date.as_bytes()[4], b'-');
        assert_eq!(date.as_bytes()[7], b'-');
    }

    #[test]
    fn test_days_to_date_epoch() {
        // Unix epoch = 1970-01-01
        let (y, m, d) = days_to_date(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_date_known() {
        // 2024-01-01 = day 19723
        let (y, m, d) = days_to_date(19723);
        assert_eq!((y, m, d), (2024, 1, 1));
    }
}
