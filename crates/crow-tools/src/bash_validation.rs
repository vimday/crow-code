//! Bash command validation engine.
//!
//! Ported from claw-code's `bash_validation.rs`. Provides a multi-pass
//! validation pipeline that classifies command intent, enforces permission
//! modes, and detects destructive operations before they reach the shell.
//!
//! ## Validation Pipeline
//!
//! 1. `validate_read_only` — blocks write/state-modifying commands in ReadOnly mode
//! 2. `validate_destructive` — flags dangerous destructive commands
//! 3. `classify_intent` — semantic classification of command purpose
//! 4. `validate_path_safety` — detects suspicious path patterns
//!
//! ## Architecture
//!
//! All validators operate on the *string* level before any shell expansion
//! occurs. This is conservative by design — we'd rather block a safe command
//! than allow a dangerous one.

use super::PermissionMode;

// ─── Result Types ───────────────────────────────────────────────────

/// Result of validating a bash command before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    /// Command is safe to execute.
    Allow,
    /// Command should be blocked with the given reason.
    Block { reason: String },
    /// Command requires user confirmation with the given warning.
    Warn { message: String },
}

/// Semantic classification of a bash command's intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandIntent {
    /// Read-only operations: ls, cat, grep, find, head, tail, wc, etc.
    ReadOnly,
    /// File system writes: cp, mv, mkdir, touch, tee, etc.
    Write,
    /// Destructive operations: rm, shred, truncate, etc.
    Destructive,
    /// Network operations: curl, wget, ssh, scp, etc.
    Network,
    /// Process management: kill, pkill, killall, etc.
    ProcessManagement,
    /// Package management: apt, brew, pip, npm, cargo install, etc.
    PackageManagement,
    /// System administration: sudo, chmod, chown, mount, etc.
    SystemAdmin,
    /// Unknown or unclassifiable command.
    Unknown,
}

// ─── Command Lists ──────────────────────────────────────────────────

/// Commands that are purely read-only and always safe.
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "ll",
    "la",
    "dir",
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "find",
    "fd",
    "locate",
    "which",
    "whereis",
    "whatis",
    "type",
    "file",
    "stat",
    "wc",
    "diff",
    "cmp",
    "md5sum",
    "sha256sum",
    "sha1sum",
    "shasum",
    "cksum",
    "du",
    "df",
    "free",
    "uptime",
    "uname",
    "hostname",
    "whoami",
    "id",
    "groups",
    "env",
    "printenv",
    "echo",
    "printf",
    "date",
    "cal",
    "bc",
    "expr",
    "test",
    "true",
    "false",
    "pwd",
    "realpath",
    "dirname",
    "basename",
    "readlink",
    "tree",
    "exa",
    "bat",
    "jq",
    "yq",
    "xq",
    "column",
    "sort",
    "uniq",
    "cut",
    "tr",
    "sed",
    "awk",
    "paste",
    "join",
    "comm",
    "tac",
    "rev",
    "fold",
    "fmt",
    "nl",
    "expand",
    "unexpand",
    "strings",
    "xxd",
    "hexdump",
    "od",
    "nm",
    "ldd",
    "objdump",
    "size",
    "readelf",
    "otool",
    "man",
    "info",
    "help",
    "history",
    // Git read-only
    "git log",
    "git status",
    "git diff",
    "git show",
    "git branch",
    "git tag",
    "git remote",
    "git stash list",
    "git blame",
    "git shortlog",
    "git describe",
    "git rev-parse",
    "git ls-files",
    "git ls-tree",
    "git config --list",
    "git config --get",
    // Cargo/Rust read-only
    "cargo check",
    "cargo test",
    "cargo bench",
    "cargo clippy",
    "cargo doc",
    "cargo tree",
    "cargo metadata",
    "cargo verify-project",
    "rustc --version",
    "rustup show",
    // Node.js read-only
    "node --version",
    "npm --version",
    "npm list",
    "npm ls",
    "npm info",
    "npm view",
    "npx --version",
    "yarn --version",
    "pnpm --version",
    // Python read-only
    "python --version",
    "python3 --version",
    "pip list",
    "pip3 list",
    "pip show",
    "pip3 show",
];

/// Commands that perform write operations.
const WRITE_COMMANDS: &[&str] = &[
    "cp", "mv", "mkdir", "rmdir", "touch", "ln", "install", "tee", "mkfifo", "mknod", "dd",
    "patch", "rsync",
];

/// Commands that are destructive (irreversible data loss).
const DESTRUCTIVE_COMMANDS: &[&str] = &["rm", "shred", "truncate", "wipefs"];

/// Commands that modify system state.
const STATE_MODIFYING_COMMANDS: &[&str] = &[
    "apt",
    "apt-get",
    "yum",
    "dnf",
    "pacman",
    "brew",
    "pip",
    "pip3",
    "npm",
    "yarn",
    "pnpm",
    "bun",
    "gem",
    "go",
    "rustup",
    "docker",
    "podman",
    "systemctl",
    "service",
    "launchctl",
    "mount",
    "umount",
];

/// Commands that manage processes.
const PROCESS_COMMANDS: &[&str] = &["kill", "pkill", "killall", "xkill", "skill"];

/// Network-related commands.
const NETWORK_COMMANDS: &[&str] = &[
    "curl",
    "wget",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "ftp",
    "telnet",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "nmap",
    "ping",
    "traceroute",
    "dig",
    "nslookup",
    "host",
];

/// System administration commands.
const SYSADMIN_COMMANDS: &[&str] = &[
    "sudo",
    "su",
    "chmod",
    "chown",
    "chgrp",
    "chattr",
    "setfacl",
    "reboot",
    "shutdown",
    "halt",
    "poweroff",
    "init",
    "useradd",
    "userdel",
    "usermod",
    "groupadd",
    "groupdel",
    "crontab",
    "at",
    "iptables",
    "ufw",
    "firewall-cmd",
    "modprobe",
    "insmod",
    "rmmod",
    "sysctl",
];

/// Shell redirection operators that indicate writes.
const WRITE_REDIRECTIONS: &[&str] = &[">", ">>", ">&"];

/// Dangerous path patterns that should trigger extra scrutiny.
const DANGEROUS_PATH_PATTERNS: &[&str] = &[
    "/etc/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/boot/",
    "/dev/",
    "/proc/",
    "/sys/",
    "/var/log/",
    "/root/",
    "~/.ssh/",
    "~/.gnupg/",
    "~/.config/",
];

// ─── Public API ─────────────────────────────────────────────────────

/// Run the full validation pipeline for a bash command.
///
/// Returns `ValidationResult::Allow` only if ALL checks pass.
/// Otherwise returns the first `Block` or `Warn` encountered.
#[must_use]
pub fn validate_command(
    command: &str,
    mode: PermissionMode,
    workspace_root: &std::path::Path,
) -> ValidationResult {
    // 1. Read-only validation
    let ro_result = validate_read_only(command, mode);
    if ro_result != ValidationResult::Allow {
        return ro_result;
    }

    // 2. Destructive command detection (warn in non-danger modes)
    let destr_result = validate_destructive(command, mode);
    if destr_result != ValidationResult::Allow {
        return destr_result;
    }

    // 3. Path safety validation
    let path_result = validate_path_safety(command, mode, workspace_root);
    if path_result != ValidationResult::Allow {
        return path_result;
    }

    ValidationResult::Allow
}

/// Classify the semantic intent of a bash command.
#[must_use]
pub fn classify_intent(command: &str) -> CommandIntent {
    let first_cmd = extract_first_command(command);

    // Check compound git commands
    if first_cmd == "git" {
        return classify_git_intent(command);
    }

    // Check compound cargo commands
    if first_cmd == "cargo" {
        return classify_cargo_intent(command);
    }

    // Direct command classification
    if READ_ONLY_COMMANDS.iter().any(|&c| c == first_cmd) {
        return CommandIntent::ReadOnly;
    }
    if DESTRUCTIVE_COMMANDS.contains(&first_cmd.as_str()) {
        return CommandIntent::Destructive;
    }
    if WRITE_COMMANDS.contains(&first_cmd.as_str()) {
        return CommandIntent::Write;
    }
    if NETWORK_COMMANDS.contains(&first_cmd.as_str()) {
        return CommandIntent::Network;
    }
    if PROCESS_COMMANDS.contains(&first_cmd.as_str()) {
        return CommandIntent::ProcessManagement;
    }
    if STATE_MODIFYING_COMMANDS.contains(&first_cmd.as_str()) {
        return CommandIntent::PackageManagement;
    }
    if SYSADMIN_COMMANDS.contains(&first_cmd.as_str()) {
        return CommandIntent::SystemAdmin;
    }

    // Check for sudo wrapping
    if first_cmd == "sudo" {
        let inner = extract_sudo_inner(command);
        if !inner.is_empty() {
            return classify_intent(inner);
        }
        return CommandIntent::SystemAdmin;
    }

    CommandIntent::Unknown
}

// ─── Validation Passes ──────────────────────────────────────────────

/// Validate that a command is allowed under read-only mode.
///
/// Corresponds to claw-code's `tools/BashTool/readOnlyValidation.ts`.
#[must_use]
fn validate_read_only(command: &str, mode: PermissionMode) -> ValidationResult {
    if mode != PermissionMode::ReadOnly {
        return ValidationResult::Allow;
    }

    let first_cmd = extract_first_command(command);

    // Check for write commands
    for &write_cmd in WRITE_COMMANDS {
        if first_cmd == write_cmd {
            return ValidationResult::Block {
                reason: format!(
                    "Command '{write_cmd}' modifies the filesystem and is not allowed in read-only mode"
                ),
            };
        }
    }

    // Check for destructive commands
    for &dest_cmd in DESTRUCTIVE_COMMANDS {
        if first_cmd == dest_cmd {
            return ValidationResult::Block {
                reason: format!(
                    "Command '{dest_cmd}' is destructive and is not allowed in read-only mode"
                ),
            };
        }
    }

    // Check for state-modifying commands
    for &state_cmd in STATE_MODIFYING_COMMANDS {
        if first_cmd == state_cmd {
            return ValidationResult::Block {
                reason: format!(
                    "Command '{state_cmd}' modifies system state and is not allowed in read-only mode"
                ),
            };
        }
    }

    // Check for process management commands
    for &proc_cmd in PROCESS_COMMANDS {
        if first_cmd == proc_cmd {
            return ValidationResult::Block {
                reason: format!(
                    "Command '{proc_cmd}' manages processes and is not allowed in read-only mode"
                ),
            };
        }
    }

    // Check for sysadmin commands
    for &admin_cmd in SYSADMIN_COMMANDS {
        if first_cmd == admin_cmd {
            return ValidationResult::Block {
                reason: format!(
                    "Command '{admin_cmd}' requires elevated privileges and is not allowed in read-only mode"
                ),
            };
        }
    }

    // Check for sudo wrapping
    if first_cmd == "sudo" {
        let inner = extract_sudo_inner(command);
        if !inner.is_empty() {
            let inner_result = validate_read_only(inner, mode);
            if inner_result != ValidationResult::Allow {
                return inner_result;
            }
        }
        return ValidationResult::Block {
            reason: "sudo is not allowed in read-only mode".to_string(),
        };
    }

    // Check for write redirections
    for &redir in WRITE_REDIRECTIONS {
        if command.contains(redir) {
            return ValidationResult::Block {
                reason: format!(
                    "Command contains write redirection '{redir}' which is not allowed in read-only mode"
                ),
            };
        }
    }

    // Check pipe chains — each segment must also be read-only
    for segment in split_pipe_chain(command) {
        let seg_cmd = extract_first_command(segment);
        if WRITE_COMMANDS.contains(&seg_cmd.as_str())
            || DESTRUCTIVE_COMMANDS.contains(&seg_cmd.as_str())
        {
            return ValidationResult::Block {
                reason: format!(
                    "Pipe segment contains write/destructive command '{seg_cmd}' which is not allowed in read-only mode"
                ),
            };
        }
    }

    ValidationResult::Allow
}

/// Detect destructive commands and warn/block depending on mode.
#[must_use]
fn validate_destructive(command: &str, mode: PermissionMode) -> ValidationResult {
    let first_cmd = extract_first_command(command);

    // Unwrap sudo
    let effective_cmd = if first_cmd == "sudo" {
        extract_first_command(extract_sudo_inner(command))
    } else {
        first_cmd
    };

    if !DESTRUCTIVE_COMMANDS.contains(&effective_cmd.as_str()) {
        return ValidationResult::Allow;
    }

    match mode {
        PermissionMode::ReadOnly => ValidationResult::Block {
            reason: format!(
                "Destructive command '{effective_cmd}' is not allowed in read-only mode"
            ),
        },
        PermissionMode::WorkspaceWrite | PermissionMode::Prompt => {
            // Check for especially dangerous patterns
            let cmd_lower = command.to_lowercase();

            // `rm -rf /` or `rm -rf ~` or `rm -rf *`
            if (cmd_lower.contains("rm") && cmd_lower.contains("-rf"))
                && (cmd_lower.contains(" /")
                    || cmd_lower.contains(" ~")
                    || cmd_lower.contains(" *"))
            {
                return ValidationResult::Block {
                    reason: format!(
                        "Extremely dangerous: '{command}' could destroy critical data. Blocked even in workspace-write mode."
                    ),
                };
            }

            ValidationResult::Warn {
                message: format!(
                    "⚠️ Destructive command detected: '{effective_cmd}'. This operation is irreversible."
                ),
            }
        }
        PermissionMode::DangerFullAccess => ValidationResult::Allow,
    }
}

/// Validate that command paths don't escape to dangerous system locations.
#[must_use]
fn validate_path_safety(
    command: &str,
    mode: PermissionMode,
    _workspace_root: &std::path::Path,
) -> ValidationResult {
    if mode == PermissionMode::DangerFullAccess {
        return ValidationResult::Allow;
    }

    // Only check write/destructive commands for path safety
    let first_cmd = extract_first_command(command);
    let is_write_or_destructive = WRITE_COMMANDS.contains(&first_cmd.as_str())
        || DESTRUCTIVE_COMMANDS.contains(&first_cmd.as_str());

    if !is_write_or_destructive {
        return ValidationResult::Allow;
    }

    for pattern in DANGEROUS_PATH_PATTERNS {
        if command.contains(pattern) {
            return ValidationResult::Warn {
                message: format!(
                    "⚠️ Command targets sensitive path pattern '{pattern}'. Ensure this is intentional."
                ),
            };
        }
    }

    ValidationResult::Allow
}

// ─── Command Parsing Utilities ──────────────────────────────────────

/// Extract the first command from a potentially complex shell expression.
///
/// Handles:
/// - Pipes: `cat file | grep foo` → `cat`
/// - Chains: `cd dir && make` → `cd`
/// - Semicolons: `echo hi; rm -rf /` → `echo`
/// - Leading env vars: `FOO=bar command` → `command`
/// - Subshells: `(command)` → `command`
#[must_use]
fn extract_first_command(command: &str) -> String {
    let trimmed = command.trim();

    // Strip leading subshell parens
    let trimmed = trimmed.strip_prefix('(').unwrap_or(trimmed);

    // Split on pipes, &&, ||, ;
    let first_segment = trimmed
        .split('|')
        .next()
        .unwrap_or(trimmed)
        .split("&&")
        .next()
        .unwrap_or(trimmed)
        .split("||")
        .next()
        .unwrap_or(trimmed)
        .split(';')
        .next()
        .unwrap_or(trimmed)
        .trim();

    // Skip leading environment variable assignments (FOO=bar BAZ=qux command)
    let mut parts = first_segment.split_whitespace();
    for part in parts.by_ref() {
        // If it looks like VAR=VALUE, skip it
        if part.contains('=') && !part.starts_with('-') && !part.starts_with('/') {
            continue;
        }
        return part.to_string();
    }

    // Fallback: return the entire segment as-is
    first_segment.to_string()
}

/// Extract the inner command from `sudo [-flags] command`.
#[must_use]
fn extract_sudo_inner(command: &str) -> &str {
    let trimmed = command.trim();
    let without_sudo = trimmed.strip_prefix("sudo").unwrap_or(trimmed).trim();

    // Skip sudo flags (they start with -)
    let mut rest = without_sudo;
    for part in without_sudo.split_whitespace() {
        if part.starts_with('-') {
            // Skip past this flag and its potential argument
            rest = without_sudo
                .strip_prefix(part)
                .unwrap_or(without_sudo)
                .trim();
        } else {
            // This is the actual command
            return rest;
        }
    }

    rest
}

/// Split a command into pipe chain segments.
fn split_pipe_chain(command: &str) -> Vec<&str> {
    command.split('|').map(str::trim).collect()
}

/// Classify git subcommand intent.
fn classify_git_intent(command: &str) -> CommandIntent {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 2 {
        return CommandIntent::Unknown;
    }

    match parts[1] {
        // Read-only git commands
        "log" | "status" | "diff" | "show" | "branch" | "tag" | "remote" | "stash" | "blame"
        | "shortlog" | "describe" | "rev-parse" | "ls-files" | "ls-tree" | "bisect" | "reflog"
        | "grep" | "count-objects" | "fsck" | "verify-pack" => CommandIntent::ReadOnly,
        // Write git commands
        "add" | "commit" | "push" | "pull" | "fetch" | "merge" | "rebase" | "cherry-pick"
        | "checkout" | "switch" | "restore" | "reset" | "revert" | "am" | "apply" | "mv" | "rm"
        | "clean" | "init" | "clone" | "submodule" => CommandIntent::Write,
        "config" => {
            // `git config --get` and `git config --list` are read-only
            if command.contains("--get") || command.contains("--list") || command.contains("-l") {
                CommandIntent::ReadOnly
            } else {
                CommandIntent::Write
            }
        }
        _ => CommandIntent::Unknown,
    }
}

/// Classify cargo subcommand intent.
fn classify_cargo_intent(command: &str) -> CommandIntent {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 2 {
        return CommandIntent::Unknown;
    }

    match parts[1] {
        // Read-only
        "check" | "test" | "bench" | "clippy" | "doc" | "tree" | "metadata" | "verify-project"
        | "version" | "search" | "info" => CommandIntent::ReadOnly,
        // Write
        "build" | "run" | "fmt" | "fix" | "clean" | "update" | "generate-lockfile" | "publish"
        | "package" | "init" | "new" | "add" | "remove" => CommandIntent::Write,
        // Package management
        "install" | "uninstall" => CommandIntent::PackageManagement,
        _ => CommandIntent::Unknown,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_first_command ────────────────────────────────────

    #[test]
    fn extracts_simple_command() {
        assert_eq!(extract_first_command("ls -la"), "ls");
    }

    #[test]
    fn extracts_from_pipe() {
        assert_eq!(extract_first_command("cat file | grep foo"), "cat");
    }

    #[test]
    fn extracts_from_chain() {
        assert_eq!(extract_first_command("cd dir && make"), "cd");
    }

    #[test]
    fn extracts_from_semicolon() {
        assert_eq!(extract_first_command("echo hi; rm -rf /"), "echo");
    }

    #[test]
    fn skips_env_var_assignments() {
        assert_eq!(
            extract_first_command("FOO=bar BAZ=qux command arg"),
            "command"
        );
    }

    #[test]
    fn handles_subshell_parens() {
        assert_eq!(extract_first_command("(echo hello)"), "echo");
    }

    // ── extract_sudo_inner ──────────────────────────────────────

    #[test]
    fn unwraps_sudo() {
        assert_eq!(extract_sudo_inner("sudo rm -rf /tmp"), "rm -rf /tmp");
    }

    #[test]
    fn unwraps_sudo_with_flags() {
        // Note: simplified sudo flag handling
        let inner = extract_sudo_inner("sudo -u root rm -rf /tmp");
        assert!(inner.contains("rm"));
    }

    // ── classify_intent ─────────────────────────────────────────

    #[test]
    fn classifies_read_only() {
        assert_eq!(classify_intent("ls -la"), CommandIntent::ReadOnly);
        assert_eq!(classify_intent("cat foo.txt"), CommandIntent::ReadOnly);
        assert_eq!(
            classify_intent("grep -r pattern ."),
            CommandIntent::ReadOnly
        );
        assert_eq!(
            classify_intent("find . -name '*.rs'"),
            CommandIntent::ReadOnly
        );
    }

    #[test]
    fn classifies_destructive() {
        assert_eq!(
            classify_intent("rm -rf /tmp/dir"),
            CommandIntent::Destructive
        );
        assert_eq!(
            classify_intent("shred secret.txt"),
            CommandIntent::Destructive
        );
    }

    #[test]
    fn classifies_write() {
        assert_eq!(classify_intent("cp src dst"), CommandIntent::Write);
        assert_eq!(classify_intent("mv old new"), CommandIntent::Write);
        assert_eq!(classify_intent("mkdir -p dir"), CommandIntent::Write);
    }

    #[test]
    fn classifies_network() {
        assert_eq!(
            classify_intent("curl https://example.com"),
            CommandIntent::Network
        );
        assert_eq!(classify_intent("wget file.tar.gz"), CommandIntent::Network);
    }

    #[test]
    fn classifies_process_management() {
        assert_eq!(
            classify_intent("kill -9 1234"),
            CommandIntent::ProcessManagement
        );
        assert_eq!(
            classify_intent("pkill firefox"),
            CommandIntent::ProcessManagement
        );
    }

    #[test]
    fn classifies_package_management() {
        assert_eq!(
            classify_intent("npm install lodash"),
            CommandIntent::PackageManagement
        );
        assert_eq!(
            classify_intent("brew install ripgrep"),
            CommandIntent::PackageManagement
        );
    }

    #[test]
    fn classifies_sysadmin() {
        assert_eq!(
            classify_intent("chmod 755 script.sh"),
            CommandIntent::SystemAdmin
        );
        assert_eq!(classify_intent("sudo anything"), CommandIntent::SystemAdmin);
    }

    #[test]
    fn classifies_git_read_only() {
        assert_eq!(
            classify_intent("git log --oneline"),
            CommandIntent::ReadOnly
        );
        assert_eq!(classify_intent("git status"), CommandIntent::ReadOnly);
        assert_eq!(classify_intent("git diff HEAD"), CommandIntent::ReadOnly);
    }

    #[test]
    fn classifies_git_write() {
        assert_eq!(classify_intent("git commit -m 'msg'"), CommandIntent::Write);
        assert_eq!(
            classify_intent("git push origin main"),
            CommandIntent::Write
        );
    }

    #[test]
    fn classifies_cargo_read_only() {
        assert_eq!(classify_intent("cargo check"), CommandIntent::ReadOnly);
        assert_eq!(classify_intent("cargo test"), CommandIntent::ReadOnly);
        assert_eq!(classify_intent("cargo clippy"), CommandIntent::ReadOnly);
    }

    #[test]
    fn classifies_cargo_write() {
        assert_eq!(classify_intent("cargo build"), CommandIntent::Write);
        assert_eq!(classify_intent("cargo fmt"), CommandIntent::Write);
    }

    // ── validate_read_only ──────────────────────────────────────

    #[test]
    fn read_only_blocks_write_commands() {
        let result = validate_read_only("cp src dst", PermissionMode::ReadOnly);
        assert!(matches!(result, ValidationResult::Block { .. }));
    }

    #[test]
    fn read_only_blocks_destructive() {
        let result = validate_read_only("rm -rf /tmp", PermissionMode::ReadOnly);
        assert!(matches!(result, ValidationResult::Block { .. }));
    }

    #[test]
    fn read_only_allows_ls() {
        let result = validate_read_only("ls -la", PermissionMode::ReadOnly);
        assert_eq!(result, ValidationResult::Allow);
    }

    #[test]
    fn read_only_blocks_redirections() {
        let result = validate_read_only("echo foo > bar.txt", PermissionMode::ReadOnly);
        assert!(matches!(result, ValidationResult::Block { .. }));
    }

    #[test]
    fn read_only_blocks_sudo_wrapping_write() {
        let result = validate_read_only("sudo rm -rf /tmp", PermissionMode::ReadOnly);
        assert!(matches!(result, ValidationResult::Block { .. }));
    }

    #[test]
    fn danger_mode_allows_everything() {
        let result = validate_read_only("rm -rf /", PermissionMode::DangerFullAccess);
        assert_eq!(result, ValidationResult::Allow);
    }

    // ── validate_destructive ────────────────────────────────────

    #[test]
    fn blocks_rm_rf_slash_in_workspace_write() {
        let result = validate_destructive("rm -rf /", PermissionMode::WorkspaceWrite);
        assert!(matches!(result, ValidationResult::Block { .. }));
    }

    #[test]
    fn warns_rm_in_workspace_write() {
        let result = validate_destructive("rm file.txt", PermissionMode::WorkspaceWrite);
        assert!(matches!(result, ValidationResult::Warn { .. }));
    }

    #[test]
    fn allows_rm_in_danger_mode() {
        let result = validate_destructive("rm -rf /", PermissionMode::DangerFullAccess);
        assert_eq!(result, ValidationResult::Allow);
    }

    // ── validate_command (full pipeline) ────────────────────────

    #[test]
    fn full_pipeline_allows_safe_read_commands() {
        let ws = std::path::Path::new("/workspace");
        assert_eq!(
            validate_command("ls -la", PermissionMode::WorkspaceWrite, ws),
            ValidationResult::Allow
        );
        assert_eq!(
            validate_command("cat foo.rs", PermissionMode::WorkspaceWrite, ws),
            ValidationResult::Allow
        );
        assert_eq!(
            validate_command("cargo test", PermissionMode::WorkspaceWrite, ws),
            ValidationResult::Allow
        );
    }

    #[test]
    fn full_pipeline_blocks_write_in_readonly() {
        let ws = std::path::Path::new("/workspace");
        let result = validate_command("cp a b", PermissionMode::ReadOnly, ws);
        assert!(matches!(result, ValidationResult::Block { .. }));
    }
}
