//! Command Safety Classifier.
//!
//! Provides structured safety classification for shell commands using
//! lexical analysis (no tree-sitter). Complements `bash_validation` with
//! a finer-grained `SafetyLevel` enum and `CommandAnalysis` that captures
//! per-program classifications, shell metacharacter usage, and composite
//! danger escalation.
//!
//! ## Design
//!
//! Each program extracted from a command string is independently classified,
//! and the overall safety level is the *maximum* (most dangerous) across all
//! programs plus any shell-level escalations (subshell expansion, redirects).

use std::collections::HashSet;

// ─── Safety Classification ─────────────────────────────────────────

/// Safety classification for a shell command, ordered from safest to most dangerous.
///
/// The ordering is used by [`SafetyLevel::escalate`] to compute the composite
/// danger level of piped or chained commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SafetyLevel {
    /// Read-only commands that are always safe (ls, cat, grep, etc.).
    Safe,
    /// Commands that modify files but within workspace (mkdir, cp, touch).
    WorkspaceWrite,
    /// Commands that could affect system state (chmod, systemctl).
    SystemWrite,
    /// Commands with network access (curl, wget, ssh).
    NetworkAccess,
    /// Potentially dangerous commands (rm, sudo, kill).
    Dangerous,
    /// Cannot determine safety — treat as dangerous.
    Unknown,
}

impl SafetyLevel {
    /// Returns `true` if this level is compatible with read-only mode.
    #[must_use]
    pub fn is_safe_for_readonly(&self) -> bool {
        matches!(self, Self::Safe)
    }

    /// Returns `true` if this level is compatible with workspace-write mode.
    #[must_use]
    pub fn is_safe_for_workspace(&self) -> bool {
        matches!(self, Self::Safe | Self::WorkspaceWrite)
    }

    /// Returns `true` if this level requires explicit user approval.
    #[must_use]
    pub fn requires_approval(&self) -> bool {
        matches!(self, Self::SystemWrite | Self::Dangerous | Self::Unknown)
    }

    /// Return the more dangerous of `self` and `other`.
    ///
    /// Uses the derived `Ord` which orders variants from Safe (lowest)
    /// to Unknown (highest).
    #[must_use]
    pub fn escalate(self, other: Self) -> Self {
        std::cmp::max(self, other)
    }
}

impl std::fmt::Display for SafetyLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Safe => "safe",
            Self::WorkspaceWrite => "workspace-write",
            Self::SystemWrite => "system-write",
            Self::NetworkAccess => "network-access",
            Self::Dangerous => "dangerous",
            Self::Unknown => "unknown",
        };
        f.write_str(label)
    }
}

// ─── Analysis Result ────────────────────────────────────────────────

/// Structured analysis of a shell command's safety characteristics.
#[derive(Debug, Clone)]
pub struct CommandAnalysis {
    /// Composite safety level (worst across all programs + shell features).
    pub level: SafetyLevel,
    /// Per-program classifications extracted from the command.
    pub programs: Vec<(String, SafetyLevel)>,
    /// Whether the command contains output redirects (`>` or `>>`).
    pub has_redirect: bool,
    /// Whether the command contains pipe operators (`|`).
    pub has_pipe: bool,
    /// Whether the command contains subshell expansion (`$(...)`, `` `...` ``, `${...}`).
    pub has_subshell: bool,
    /// Whether the command is backgrounded with `&`.
    pub has_background: bool,
    /// Human-readable reasons explaining the classification.
    pub reasons: Vec<String>,
}

impl CommandAnalysis {
    /// Convenience: check if overall level requires approval.
    #[must_use]
    pub fn requires_approval(&self) -> bool {
        self.level.requires_approval()
    }
}

// ─── Public API ─────────────────────────────────────────────────────

/// Analyze a shell command string and determine its safety level.
///
/// Performs lexical analysis to detect shell metacharacters, extract
/// program names from pipe/chain segments, and classify each program
/// independently.
#[must_use]
pub fn classify_command(cmd: &str) -> CommandAnalysis {
    let programs = extract_programs(cmd);
    let mut analysis = CommandAnalysis {
        level: SafetyLevel::Safe,
        programs: Vec::new(),
        has_redirect: false,
        has_pipe: false,
        has_subshell: false,
        has_background: false,
        reasons: Vec::new(),
    };

    // Check for subshell expansion — makes the command opaque.
    if contains_subshell_expansion(cmd) {
        analysis.has_subshell = true;
        analysis.level = analysis.level.escalate(SafetyLevel::Unknown);
        analysis.reasons.push("Contains subshell expansion".into());
    }

    // Check for output redirects.
    if contains_redirect(cmd) {
        analysis.has_redirect = true;
        analysis.level = analysis.level.escalate(SafetyLevel::WorkspaceWrite);
        analysis.reasons.push("Contains output redirect".into());
    }

    // Check for pipes.
    if cmd.contains('|') {
        analysis.has_pipe = true;
    }

    // Check for backgrounding.
    if is_backgrounded(cmd) {
        analysis.has_background = true;
        analysis.reasons.push("Runs in background".into());
    }

    // Classify each extracted program.
    for program in &programs {
        let level = classify_program(program);
        analysis.programs.push((program.clone(), level));
        if level != SafetyLevel::Safe {
            analysis
                .reasons
                .push(format!("'{program}' classified as {level}"));
        }
        analysis.level = analysis.level.escalate(level);
    }

    analysis
}

// ─── Program Classification ─────────────────────────────────────────

/// Classify a single program name by safety level.
#[must_use]
fn classify_program(name: &str) -> SafetyLevel {
    // Use local `const` slices (cheaper than allocating HashSets every call)
    // and fall through to HashSet membership only for the check.

    static SAFE: &[&str] = &[
        "cat", "echo", "head", "tail", "wc", "sort", "uniq", "grep", "rg", "find", "fd", "ls",
        "tree", "pwd", "whoami", "date", "env", "printenv", "which", "type", "file", "stat", "du",
        "df", "uname", "hostname", "diff", "cmp", "jq", "yq", "sed", "awk", "tr", "cut", "tee",
        "basename", "dirname", "realpath", "readlink", "true", "false", "test", "[", "bat", "exa",
        "less", "more", "man", "help", "printf", "cal", "bc", "expr", "strings", "xxd", "hexdump",
        "od", "fmt", "fold", "nl", "rev", "tac", "paste", "join", "comm", "expand", "unexpand",
        "column",
    ];

    static WORKSPACE_WRITE: &[&str] = &["mkdir", "touch", "cp", "mv", "ln", "patch", "install"];

    static BUILD_TOOLS: &[&str] = &[
        "cargo", "rustc", "python3", "python", "node", "npm", "npx", "pip", "pip3", "yarn", "pnpm",
        "bun", "git",
    ];

    static DANGEROUS: &[&str] = &[
        "rm", "rmdir", "shred", "truncate", "sudo", "su", "doas", "kill", "killall", "pkill",
        "xkill", "reboot", "shutdown", "halt", "poweroff", "mkfs", "fdisk", "dd",
    ];

    static SYSTEM_WRITE: &[&str] = &[
        "chmod",
        "chown",
        "chgrp",
        "chattr",
        "iptables",
        "ufw",
        "firewall-cmd",
        "systemctl",
        "launchctl",
        "service",
        "mount",
        "umount",
        "crontab",
        "at",
        "useradd",
        "userdel",
        "usermod",
        "groupadd",
        "groupdel",
        "sysctl",
        "modprobe",
        "insmod",
        "rmmod",
    ];

    static NETWORK: &[&str] = &[
        "curl",
        "wget",
        "ssh",
        "scp",
        "sftp",
        "rsync",
        "nc",
        "netcat",
        "ncat",
        "socat",
        "nmap",
        "ftp",
        "telnet",
        "ping",
        "traceroute",
        "dig",
        "nslookup",
        "host",
    ];

    // Build HashSets for O(1) lookup.
    // These are small enough that the allocation is negligible.
    let safe_set: HashSet<&str> = SAFE.iter().copied().collect();
    let ws_write_set: HashSet<&str> = WORKSPACE_WRITE.iter().copied().collect();
    let build_set: HashSet<&str> = BUILD_TOOLS.iter().copied().collect();
    let dangerous_set: HashSet<&str> = DANGEROUS.iter().copied().collect();
    let system_set: HashSet<&str> = SYSTEM_WRITE.iter().copied().collect();
    let network_set: HashSet<&str> = NETWORK.iter().copied().collect();

    if safe_set.contains(name) {
        SafetyLevel::Safe
    } else if ws_write_set.contains(name) {
        SafetyLevel::WorkspaceWrite
    } else if build_set.contains(name) {
        // Build tools straddle safe/write — default to workspace-write
        // since they often produce artifacts.
        SafetyLevel::WorkspaceWrite
    } else if system_set.contains(name) {
        SafetyLevel::SystemWrite
    } else if network_set.contains(name) {
        SafetyLevel::NetworkAccess
    } else if dangerous_set.contains(name) {
        SafetyLevel::Dangerous
    } else {
        SafetyLevel::Unknown
    }
}

// ─── Lexical Utilities ──────────────────────────────────────────────

/// Check for subshell / variable expansion patterns that make static
/// analysis unreliable.
fn contains_subshell_expansion(cmd: &str) -> bool {
    cmd.contains("$(") || cmd.contains('`') || cmd.contains("${")
}

/// Check for output redirect operators.
fn contains_redirect(cmd: &str) -> bool {
    // Must distinguish `>` redirect from `->` in Rust code snippets passed as args,
    // and from `>=` comparisons. A simple heuristic: any `>` not preceded by `-`
    // or `!` and not part of `>=`.
    let bytes = cmd.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'>' {
            // Check it's not `->` or `>=`
            if i > 0 && bytes[i - 1] == b'-' {
                continue;
            }
            return true;
        }
    }
    false
}

/// Check if the command is backgrounded (trailing `&` not part of `&&`).
fn is_backgrounded(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    trimmed.ends_with('&') && !trimmed.ends_with("&&")
}

/// Extract program names from a shell command string.
///
/// Splits on pipes (`|`), semicolons (`;`), `&&`, `||` to isolate individual
/// commands, then extracts the first word from each segment (skipping
/// environment variable assignments like `FOO=bar`).
fn extract_programs(cmd: &str) -> Vec<String> {
    let mut programs = Vec::new();

    // Rough split: replace `&&` and `||` with `;` first to simplify,
    // then split on `;` and `|`.
    let normalized = cmd.replace("&&", ";").replace("||", ";");

    for segment in normalized.split(['|', ';']) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }
        if let Some(program) = extract_first_word(seg) {
            programs.push(program);
        }
    }

    programs
}

/// Extract the first non-env-assignment word from a command segment.
///
/// Skips leading `(` and env assignments (`VAR=val`).
fn extract_first_word(segment: &str) -> Option<String> {
    let stripped = segment.trim().strip_prefix('(').unwrap_or(segment.trim());

    for word in stripped.split_whitespace() {
        // Skip env variable assignments (FOO=bar)
        if word.contains('=') && !word.starts_with('-') && !word.starts_with('/') {
            continue;
        }
        return Some(word.to_string());
    }
    None
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ── SafetyLevel ordering ────────────────────────────────────

    #[test]
    fn safety_level_ordering() {
        assert!(SafetyLevel::Safe < SafetyLevel::WorkspaceWrite);
        assert!(SafetyLevel::WorkspaceWrite < SafetyLevel::SystemWrite);
        assert!(SafetyLevel::SystemWrite < SafetyLevel::NetworkAccess);
        assert!(SafetyLevel::NetworkAccess < SafetyLevel::Dangerous);
        assert!(SafetyLevel::Dangerous < SafetyLevel::Unknown);
    }

    #[test]
    fn escalate_picks_higher() {
        assert_eq!(
            SafetyLevel::Safe.escalate(SafetyLevel::Dangerous),
            SafetyLevel::Dangerous,
        );
        assert_eq!(
            SafetyLevel::Unknown.escalate(SafetyLevel::Safe),
            SafetyLevel::Unknown,
        );
        assert_eq!(
            SafetyLevel::NetworkAccess.escalate(SafetyLevel::WorkspaceWrite),
            SafetyLevel::NetworkAccess,
        );
    }

    // ── SafetyLevel predicates ──────────────────────────────────

    #[test]
    fn safe_for_readonly() {
        assert!(SafetyLevel::Safe.is_safe_for_readonly());
        assert!(!SafetyLevel::WorkspaceWrite.is_safe_for_readonly());
        assert!(!SafetyLevel::Dangerous.is_safe_for_readonly());
    }

    #[test]
    fn safe_for_workspace() {
        assert!(SafetyLevel::Safe.is_safe_for_workspace());
        assert!(SafetyLevel::WorkspaceWrite.is_safe_for_workspace());
        assert!(!SafetyLevel::SystemWrite.is_safe_for_workspace());
        assert!(!SafetyLevel::Dangerous.is_safe_for_workspace());
    }

    #[test]
    fn requires_approval_levels() {
        assert!(!SafetyLevel::Safe.requires_approval());
        assert!(!SafetyLevel::WorkspaceWrite.requires_approval());
        assert!(!SafetyLevel::NetworkAccess.requires_approval());
        assert!(SafetyLevel::SystemWrite.requires_approval());
        assert!(SafetyLevel::Dangerous.requires_approval());
        assert!(SafetyLevel::Unknown.requires_approval());
    }

    // ── Safe commands ───────────────────────────────────────────

    #[test]
    fn classifies_safe_commands() {
        for cmd in &["ls -la", "cat foo.txt", "echo hello", "grep -r pattern ."] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::Safe,
                "Expected Safe for '{cmd}'",
            );
        }
    }

    #[test]
    fn classifies_more_safe_commands() {
        for cmd in &[
            "head -20 file",
            "tail -f log",
            "wc -l file",
            "find . -name '*.rs'",
        ] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::Safe,
                "Expected Safe for '{cmd}'",
            );
        }
    }

    // ── Workspace write commands ────────────────────────────────

    #[test]
    fn classifies_workspace_write_commands() {
        for cmd in &["mkdir -p src/new", "cp file1 file2", "touch newfile.rs"] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::WorkspaceWrite,
                "Expected WorkspaceWrite for '{cmd}'",
            );
        }
    }

    #[test]
    fn classifies_build_tools_as_workspace_write() {
        for cmd in &["cargo build", "npm install", "git add ."] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::WorkspaceWrite,
                "Expected WorkspaceWrite for '{cmd}'",
            );
        }
    }

    // ── Dangerous commands ──────────────────────────────────────

    #[test]
    fn classifies_dangerous_commands() {
        for cmd in &["rm -rf /tmp", "sudo apt-get update", "kill -9 1234"] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::Dangerous,
                "Expected Dangerous for '{cmd}'",
            );
        }
    }

    #[test]
    fn classifies_more_dangerous_commands() {
        for cmd in &[
            "killall firefox",
            "shutdown -h now",
            "dd if=/dev/zero of=/dev/sda",
        ] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::Dangerous,
                "Expected Dangerous for '{cmd}'",
            );
        }
    }

    // ── Network commands ────────────────────────────────────────

    #[test]
    fn classifies_network_commands() {
        for cmd in &[
            "curl https://example.com",
            "wget file.tar.gz",
            "ssh user@host",
        ] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::NetworkAccess,
                "Expected NetworkAccess for '{cmd}'",
            );
        }
    }

    // ── System write commands ───────────────────────────────────

    #[test]
    fn classifies_system_write_commands() {
        for cmd in &[
            "chmod 755 script.sh",
            "chown root:root file",
            "systemctl restart nginx",
        ] {
            let analysis = classify_command(cmd);
            assert_eq!(
                analysis.level,
                SafetyLevel::SystemWrite,
                "Expected SystemWrite for '{cmd}'",
            );
        }
    }

    // ── Subshell expansion ──────────────────────────────────────

    #[test]
    fn detects_subshell_dollar_paren() {
        let analysis = classify_command("echo $(whoami)");
        assert!(analysis.has_subshell);
        assert_eq!(analysis.level, SafetyLevel::Unknown);
    }

    #[test]
    fn detects_subshell_backtick() {
        let analysis = classify_command("echo `date`");
        assert!(analysis.has_subshell);
        assert_eq!(analysis.level, SafetyLevel::Unknown);
    }

    #[test]
    fn detects_subshell_dollar_brace() {
        let analysis = classify_command("echo ${HOME}");
        assert!(analysis.has_subshell);
        assert_eq!(analysis.level, SafetyLevel::Unknown);
    }

    // ── Pipes and redirects ─────────────────────────────────────

    #[test]
    fn detects_pipes() {
        let analysis = classify_command("cat file | grep foo | wc -l");
        assert!(analysis.has_pipe);
        assert_eq!(analysis.level, SafetyLevel::Safe);
        assert_eq!(analysis.programs.len(), 3);
    }

    #[test]
    fn detects_redirects() {
        let analysis = classify_command("echo hello > output.txt");
        assert!(analysis.has_redirect);
        assert!(analysis.level >= SafetyLevel::WorkspaceWrite);
    }

    #[test]
    fn detects_append_redirect() {
        let analysis = classify_command("echo hello >> output.txt");
        assert!(analysis.has_redirect);
    }

    #[test]
    fn detects_background() {
        let analysis = classify_command("sleep 10 &");
        assert!(analysis.has_background);
    }

    #[test]
    fn double_ampersand_not_background() {
        let analysis = classify_command("ls && echo done");
        assert!(!analysis.has_background);
    }

    // ── Combined / piped commands ───────────────────────────────

    #[test]
    fn pipe_escalates_to_highest_danger() {
        // cat is Safe, but rm is Dangerous → overall Dangerous
        let analysis = classify_command("cat file | rm -rf /tmp");
        assert_eq!(analysis.level, SafetyLevel::Dangerous);
    }

    #[test]
    fn chain_escalates_to_highest_danger() {
        // echo is Safe, curl is NetworkAccess → overall NetworkAccess
        let analysis = classify_command("echo start && curl http://example.com");
        assert_eq!(analysis.level, SafetyLevel::NetworkAccess);
    }

    #[test]
    fn semicolon_chain_escalates() {
        // ls is Safe, sudo is Dangerous → overall Dangerous
        let analysis = classify_command("ls; sudo reboot");
        assert_eq!(analysis.level, SafetyLevel::Dangerous);
    }

    // ── Unknown commands ────────────────────────────────────────

    #[test]
    fn unknown_program_is_unknown() {
        let analysis = classify_command("my_custom_script --flag");
        assert_eq!(analysis.level, SafetyLevel::Unknown);
    }

    // ── extract_programs ────────────────────────────────────────

    #[test]
    fn extracts_single_program() {
        let progs = extract_programs("ls -la");
        assert_eq!(progs, vec!["ls"]);
    }

    #[test]
    fn extracts_piped_programs() {
        let progs = extract_programs("cat file | grep foo | wc -l");
        assert_eq!(progs, vec!["cat", "grep", "wc"]);
    }

    #[test]
    fn extracts_chained_programs() {
        let progs = extract_programs("mkdir -p dir && cd dir && touch file");
        assert_eq!(progs, vec!["mkdir", "cd", "touch"]);
    }

    #[test]
    fn skips_env_assignments() {
        let progs = extract_programs("FOO=bar BAZ=1 cargo test");
        assert_eq!(progs, vec!["cargo"]);
    }

    #[test]
    fn handles_empty_input() {
        let progs = extract_programs("");
        assert!(progs.is_empty());

        let analysis = classify_command("");
        assert_eq!(analysis.level, SafetyLevel::Safe);
        assert!(analysis.programs.is_empty());
    }

    // ── Display ─────────────────────────────────────────────────

    #[test]
    fn display_safety_levels() {
        assert_eq!(SafetyLevel::Safe.to_string(), "safe");
        assert_eq!(SafetyLevel::Dangerous.to_string(), "dangerous");
        assert_eq!(SafetyLevel::Unknown.to_string(), "unknown");
    }

    // ── Reasons populated ───────────────────────────────────────

    #[test]
    fn reasons_populated_for_dangerous() {
        let analysis = classify_command("rm -rf /tmp");
        assert!(!analysis.reasons.is_empty());
        assert!(analysis.reasons.iter().any(|r| r.contains("rm")));
    }

    #[test]
    fn reasons_populated_for_subshell() {
        let analysis = classify_command("echo $(whoami)");
        assert!(analysis.reasons.iter().any(|r| r.contains("subshell")));
    }

    #[test]
    fn reasons_populated_for_redirect() {
        let analysis = classify_command("echo hello > file");
        assert!(analysis.reasons.iter().any(|r| r.contains("redirect")));
    }
}
