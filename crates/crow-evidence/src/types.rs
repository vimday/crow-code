//! Core data types for the evidence contract.

use std::time::Duration;

// ─── Confidence ─────────────────────────────────────────────────────

/// Confidence tier for intelligence-derived conclusions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// No signal at all (e.g. language not recognized).
    None,
    /// Heuristic guess (e.g. grep-based).
    Low,
    /// Partial signal (e.g. Tree-sitter parse without LSP).
    Medium,
    /// Strong signal (e.g. LSP diagnostic + full parse).
    High,
}

// ─── Test Runs ──────────────────────────────────────────────────────

/// Outcome of a single test execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Passed,
    Failed,
    Skipped,
    TimedOut,
}

/// A structured record of a single test/build run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    /// The command that was executed (e.g. "cargo test").
    pub command: String,
    /// Overall outcome.
    pub outcome: TestOutcome,
    /// Number of individual test cases that passed.
    pub passed: usize,
    /// Number of individual test cases that failed.
    pub failed: usize,
    /// Number of individual test cases that were skipped.
    pub skipped: usize,
    /// Wall-clock duration of the run.
    pub duration: Duration,
    /// Truncated log (ACI-pruned). Head + tail only.
    pub truncated_log: String,
}

/// Whether this was a selective or full test suite run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestScope {
    /// Only affected tests were run.
    Selective,
    /// Full test suite was run.
    Full,
}

// ─── Risk Flags ─────────────────────────────────────────────────────

/// Semantic risk annotation attached to a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFlag {
    /// Machine-readable risk category.
    pub kind: RiskKind,
    /// Human-readable explanation of why this is risky.
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskKind {
    /// Touches authentication, authorization, or crypto code.
    Security,
    /// Deletes significant logic (> threshold lines of non-comment code).
    LargeDeletion,
    /// Modifies a public API surface.
    PublicApiChange,
    /// Touches configuration or environment handling.
    ConfigChange,
    /// Involves concurrent/async code paths.
    ConcurrencyChange,
    /// Catch-all for domain-specific risks.
    Other,
}

// ─── Evidence Matrix ────────────────────────────────────────────────

/// The multidimensional evidence bundle that replaces a single 0-100 score.
/// Every field traces back to a concrete command, log, or parser output.
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceMatrix {
    /// All verification runs performed (may be empty if no verifier was available).
    pub compile_runs: Vec<TestRun>,
    /// Scope of testing: selective vs full.
    pub test_scope: Option<TestScope>,
    /// Whether known baseline results exist for comparison.
    pub has_known_baseline: bool,
    /// Whether linting passed cleanly.
    pub lints_clean: bool,
    /// Intelligence subsystem confidence in gathered context.
    pub intelligence_confidence: Confidence,
    /// Semantic risk annotations.
    pub risk_flags: Vec<RiskFlag>,
}

impl EvidenceMatrix {
    /// Create an empty evidence matrix (no evidence gathered yet).
    pub fn empty() -> Self {
        Self {
            compile_runs: vec![],
            test_scope: None,
            has_known_baseline: false,
            lints_clean: false,
            intelligence_confidence: Confidence::None,
            risk_flags: vec![],
        }
    }

    /// True only when evidence is *sufficient and positive*.
    ///
    /// Requirements (all must hold):
    /// - At least one compile run, all passed
    /// - Lints clean
    /// - Test scope is known (not `None`)
    /// - Intelligence confidence is at least `Medium`
    /// - A known baseline exists for comparison
    /// - No severe risk flags (Security, LargeDeletion)
    pub fn is_all_green(&self) -> bool {
        let has_runs = !self.compile_runs.is_empty();
        let all_passed = has_runs
            && self.compile_runs.iter().all(|r| r.outcome == TestOutcome::Passed);
        let scope_known = self.test_scope.is_some();
        let sufficient_intel = self.intelligence_confidence >= Confidence::Medium;
        let no_severe_risks = !self.risk_flags.iter().any(|f| {
            matches!(f.kind, RiskKind::Security | RiskKind::LargeDeletion)
        });
        all_passed
            && self.lints_clean
            && scope_known
            && sufficient_intel
            && self.has_known_baseline
            && no_severe_risks
    }

    /// Count total failures across all runs.
    pub fn total_failures(&self) -> usize {
        self.compile_runs.iter().map(|r| r.failed).sum()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_passing_run() -> TestRun {
        TestRun {
            command: "cargo test".into(),
            outcome: TestOutcome::Passed,
            passed: 42,
            failed: 0,
            skipped: 0,
            duration: Duration::from_millis(1200),
            truncated_log: String::new(),
        }
    }

    fn make_failing_run() -> TestRun {
        TestRun {
            command: "cargo test".into(),
            outcome: TestOutcome::Failed,
            passed: 30,
            failed: 3,
            skipped: 1,
            duration: Duration::from_millis(800),
            truncated_log: "error[E0308]: mismatched types".into(),
        }
    }

    #[test]
    fn empty_matrix_is_not_green() {
        let m = EvidenceMatrix::empty();
        assert!(!m.is_all_green());
    }

    #[test]
    fn all_green_with_passing_runs() {
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: Some(TestScope::Full),
            has_known_baseline: true,
            lints_clean: true,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![],
        };
        assert!(m.is_all_green());
    }

    #[test]
    fn not_green_when_lints_dirty() {
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: Some(TestScope::Full),
            has_known_baseline: true,
            lints_clean: false,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![],
        };
        assert!(!m.is_all_green());
    }

    #[test]
    fn not_green_with_security_risk() {
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: Some(TestScope::Full),
            has_known_baseline: true,
            lints_clean: true,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![RiskFlag {
                kind: RiskKind::Security,
                description: "Touches JWT validation".into(),
            }],
        };
        assert!(!m.is_all_green());
    }

    #[test]
    fn total_failures_counts_across_runs() {
        let m = EvidenceMatrix {
            compile_runs: vec![make_failing_run(), make_failing_run()],
            test_scope: Some(TestScope::Selective),
            has_known_baseline: false,
            lints_clean: true,
            intelligence_confidence: Confidence::Medium,
            risk_flags: vec![],
        };
        assert_eq!(m.total_failures(), 6);
    }

    #[test]
    fn confidence_ordering() {
        assert!(Confidence::None < Confidence::Low);
        assert!(Confidence::Low < Confidence::Medium);
        assert!(Confidence::Medium < Confidence::High);
    }

    #[test]
    fn config_change_does_not_block_green() {
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: Some(TestScope::Full),
            has_known_baseline: true,
            lints_clean: true,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![RiskFlag {
                kind: RiskKind::ConfigChange,
                description: "Changed env var names".into(),
            }],
        };
        // ConfigChange is not a severe risk, should still be green
        assert!(m.is_all_green());
    }

    #[test]
    fn not_green_with_selective_no_baseline() {
        // Passes all runs, but selective scope + no baseline = insufficient
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: Some(TestScope::Selective),
            has_known_baseline: false,
            lints_clean: true,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![],
        };
        assert!(!m.is_all_green());
    }

    #[test]
    fn not_green_with_no_intelligence() {
        // Everything else green, but intelligence confidence is None
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: Some(TestScope::Full),
            has_known_baseline: true,
            lints_clean: true,
            intelligence_confidence: Confidence::None,
            risk_flags: vec![],
        };
        assert!(!m.is_all_green());
    }

    #[test]
    fn not_green_with_no_scope() {
        // Passed run but scope is unknown
        let m = EvidenceMatrix {
            compile_runs: vec![make_passing_run()],
            test_scope: None,
            has_known_baseline: true,
            lints_clean: true,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![],
        };
        assert!(!m.is_all_green());
    }
}
