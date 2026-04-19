//! Core data types for the probe contract.

use std::path::PathBuf;

// ─── Language Tier ──────────────────────────────────────────────────

/// Language classification that determines intelligence confidence ceiling.
/// A Tier-1 language has full LSP support; Tier-3 is best-effort grep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LanguageTier {
    /// Unknown or unsupported language. Intelligence = grep only.
    Tier3,
    /// Partial support (e.g. Tree-sitter parse, no LSP).
    Tier2,
    /// Full support (Tree-sitter + LSP + type checking).
    Tier1,
}

/// Detected primary language of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLanguage {
    /// Language identifier (e.g. "rust", "typescript", "python").
    pub name: String,
    /// Intelligence tier this language falls into.
    pub tier: LanguageTier,
}

// ─── Verification Candidates ────────────────────────────────────────

/// Confidence level of a detected verification command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeConfidence {
    /// Guessed from file presence alone.
    Inferred,
    /// Found in a manifest (e.g. Cargo.toml, package.json scripts).
    ManifestBacked,
    /// Validated by a dry-run or previous successful execution.
    Validated,
}

/// A structured command representation that avoids shell-parsing ambiguity.
/// This is the cross-crate contract consumed by `crow-verifier` and `crow-replay`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCommand {
    /// The program to execute (e.g. "cargo", "npm", "python").
    pub program: String,
    /// Arguments (e.g. ["test", "--workspace"]).
    pub args: Vec<String>,
    /// Optional working directory override (relative to workspace root).
    pub cwd: Option<String>,
}

impl VerificationCommand {
    /// Convenience constructor for simple program + args.
    pub fn new(program: impl Into<String>, args: Vec<&str>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(String::from).collect(),
            cwd: None,
        }
    }

    /// Format as a human-readable command string (for display only, not execution).
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// A candidate verification command discovered by the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCandidate {
    /// The structured command to run.
    pub command: VerificationCommand,
    /// What kind of verification this provides.
    pub kind: VerificationKind,
    /// How confident we are this command is correct.
    pub confidence: ProbeConfidence,
    /// Where the evidence came from (e.g. "Cargo.toml", "package.json").
    pub evidence_source: String,
}

/// Category of verification a command provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationKind {
    Build,
    Test,
    Lint,
    TypeCheck,
}

// ─── Filter Spec ────────────────────────────────────────────────────

/// Extracted ignore/filter rules for the workspace.
/// Kept as plain patterns rather than leaking third-party types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSpec {
    /// Glob patterns to ignore (sourced from .gitignore, etc.).
    pub ignore_patterns: Vec<String>,
    /// Directories that are known build artifacts (node_modules, target, etc.).
    pub artifact_dirs: Vec<String>,
}

impl FilterSpec {
    pub fn empty() -> Self {
        Self {
            ignore_patterns: vec![],
            artifact_dirs: vec![],
        }
    }
}

// ─── Project Profile ────────────────────────────────────────────────

/// The complete recon output from a workspace probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectProfile {
    /// Detected primary language and its intelligence tier.
    pub primary_lang: DetectedLanguage,
    /// Root of the workspace (may differ from CWD in monorepos).
    pub workspace_root: PathBuf,
    /// Candidate verification commands, sorted by confidence descending.
    pub verification_candidates: Vec<VerificationCandidate>,
    /// Filter rules extracted from the workspace.
    pub ignore_spec: FilterSpec,
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tier_ordering() {
        assert!(LanguageTier::Tier3 < LanguageTier::Tier2);
        assert!(LanguageTier::Tier2 < LanguageTier::Tier1);
    }

    #[test]
    fn probe_confidence_ordering() {
        assert!(ProbeConfidence::Inferred < ProbeConfidence::ManifestBacked);
        assert!(ProbeConfidence::ManifestBacked < ProbeConfidence::Validated);
    }

    #[test]
    fn rust_project_profile() {
        let profile = ProjectProfile {
            primary_lang: DetectedLanguage {
                name: "rust".into(),
                tier: LanguageTier::Tier1,
            },
            workspace_root: PathBuf::from("/home/user/crow-code"),
            verification_candidates: vec![
                VerificationCandidate {
                    command: VerificationCommand::new("cargo", vec!["test"]),
                    kind: VerificationKind::Test,
                    confidence: ProbeConfidence::ManifestBacked,
                    evidence_source: "Cargo.toml".into(),
                },
                VerificationCandidate {
                    command: VerificationCommand::new("cargo", vec!["build"]),
                    kind: VerificationKind::Build,
                    confidence: ProbeConfidence::ManifestBacked,
                    evidence_source: "Cargo.toml".into(),
                },
                VerificationCandidate {
                    command: VerificationCommand::new("cargo", vec!["clippy"]),
                    kind: VerificationKind::Lint,
                    confidence: ProbeConfidence::Inferred,
                    evidence_source: "Cargo.toml (inferred)".into(),
                },
            ],
            ignore_spec: FilterSpec {
                ignore_patterns: vec!["target/".into(), "*.swp".into()],
                artifact_dirs: vec!["target".into()],
            },
        };
        assert_eq!(profile.primary_lang.tier, LanguageTier::Tier1);
        assert_eq!(profile.verification_candidates.len(), 3);
        assert_eq!(
            profile.verification_candidates[0].command.display(),
            "cargo test"
        );
    }

    #[test]
    fn node_project_profile() {
        let profile = ProjectProfile {
            primary_lang: DetectedLanguage {
                name: "typescript".into(),
                tier: LanguageTier::Tier2,
            },
            workspace_root: PathBuf::from("/home/user/web-app"),
            verification_candidates: vec![VerificationCandidate {
                command: VerificationCommand::new("npm", vec!["test"]),
                kind: VerificationKind::Test,
                confidence: ProbeConfidence::ManifestBacked,
                evidence_source: "package.json scripts.test".into(),
            }],
            ignore_spec: FilterSpec {
                ignore_patterns: vec!["node_modules/".into(), "dist/".into()],
                artifact_dirs: vec!["node_modules".into(), "dist".into()],
            },
        };
        assert_eq!(profile.primary_lang.tier, LanguageTier::Tier2);
        assert_eq!(profile.ignore_spec.artifact_dirs.len(), 2);
    }

    #[test]
    fn empty_filter_spec() {
        let spec = FilterSpec::empty();
        assert!(spec.ignore_patterns.is_empty());
        assert!(spec.artifact_dirs.is_empty());
    }

    #[test]
    fn multiple_candidates_sorted_by_confidence() {
        let mut candidates = [
            VerificationCandidate {
                command: VerificationCommand::new("make", vec!["test"]),
                kind: VerificationKind::Test,
                confidence: ProbeConfidence::Inferred,
                evidence_source: "Makefile".into(),
            },
            VerificationCandidate {
                command: VerificationCommand::new("cargo", vec!["test"]),
                kind: VerificationKind::Test,
                confidence: ProbeConfidence::Validated,
                evidence_source: "Cargo.toml".into(),
            },
        ];
        candidates.sort_by_key(|b| std::cmp::Reverse(b.confidence));
        assert_eq!(candidates[0].confidence, ProbeConfidence::Validated);
        assert_eq!(candidates[1].confidence, ProbeConfidence::Inferred);
    }

    #[test]
    fn command_with_cwd_override() {
        let cmd = VerificationCommand {
            program: "python".into(),
            args: vec!["-m".into(), "pytest".into()],
            cwd: Some("tests/".into()),
        };
        assert_eq!(cmd.display(), "python -m pytest");
        assert_eq!(cmd.cwd, Some("tests/".into()));
    }
}
