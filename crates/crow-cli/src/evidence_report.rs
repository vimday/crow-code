//! Evidence Report — the user-facing proof bundle.
//!
//! This module turns crow's internal safety pipeline (snapshot anchoring,
//! plan hydration, preflight verification, evidence matrix) into a
//! structured, human-readable report. This is crow's primary product
//! differentiator: making trust *visible*.

use crow_evidence::types::{EvidenceMatrix, RiskKind};
use crow_patch::{Confidence, EditOp, IntentPlan, SnapshotId};
use std::fmt;

// ─── Report Structures ─────────────────────────────────────────────

/// Complete evidence report for a plan execution or preview.
#[derive(Debug)]
pub struct EvidenceReport {
    pub recon: ReconSummary,
    pub compilation: CompilationSummary,
    pub hydration: HydrationSummary,
    pub preflight: PreflightSummary,
    pub verdict: Verdict,
}

#[derive(Debug)]
pub struct ReconSummary {
    pub language: String,
    pub tier: String,
    pub snapshot_id: SnapshotId,
    pub files_scanned: usize,
    pub manifests: Vec<String>,
}

#[derive(Debug)]
pub struct CompilationSummary {
    pub rationale: String,
    pub confidence: Confidence,
    pub modify_count: usize,
    pub create_count: usize,
    pub delete_count: usize,
    pub rename_count: usize,
}

impl CompilationSummary {
    pub fn from_plan(plan: &IntentPlan) -> Self {
        let mut modify_count = 0;
        let mut create_count = 0;
        let mut delete_count = 0;
        let mut rename_count = 0;
        for op in &plan.operations {
            match op {
                EditOp::Modify { .. } => modify_count += 1,
                EditOp::Create { .. } => create_count += 1,
                EditOp::Delete { .. } => delete_count += 1,
                EditOp::Rename { .. } => rename_count += 1,
            }
        }
        Self {
            rationale: plan.rationale.clone(),
            confidence: plan.confidence,
            modify_count,
            create_count,
            delete_count,
            rename_count,
        }
    }

    pub fn total_ops(&self) -> usize {
        self.modify_count + self.create_count + self.delete_count + self.rename_count
    }
}

#[derive(Debug)]
pub struct HydrationSummary {
    pub snapshot_verified: bool,
    pub hashes_matched: usize,
    pub hashes_total: usize,
    pub drift_warnings: Vec<String>,
}

#[derive(Debug)]
pub enum PreflightOutcome {
    Clean { duration_secs: f64 },
    Errors { count: usize, summary: String },
    Skipped { reason: String },
}

#[derive(Debug)]
pub struct PreflightSummary {
    pub language: String,
    pub outcome: PreflightOutcome,
}

/// The final verdict — what should happen to this plan.
#[derive(Debug)]
pub enum Verdict {
    /// All evidence is green. Safe for automatic application.
    AutoApply {
        evidence: EvidenceMatrix,
    },
    /// Evidence is mixed. Human review recommended before application.
    ReviewRequired {
        reason: String,
        evidence: EvidenceMatrix,
    },
    /// High-risk flags detected. Escalate with full evidence bundle.
    Escalate {
        risk_flags: Vec<String>,
        evidence: EvidenceMatrix,
    },
}

impl Verdict {
    /// Compute a verdict from an evidence matrix.
    pub fn from_evidence(evidence: EvidenceMatrix) -> Self {
        // Check for severe risk flags
        let severe_risks: Vec<String> = evidence
            .risk_flags
            .iter()
            .filter(|f| matches!(f.kind, RiskKind::Security | RiskKind::LargeDeletion))
            .map(|f| f.description.clone())
            .collect();

        if !severe_risks.is_empty() {
            return Verdict::Escalate {
                risk_flags: severe_risks,
                evidence,
            };
        }

        if evidence.is_all_green() {
            Verdict::AutoApply { evidence }
        } else {
            let reason = if evidence.compile_runs.is_empty() {
                "No verification runs completed".to_string()
            } else if !evidence.lints_clean {
                "Lint warnings detected".to_string()
            } else if evidence.intelligence_confidence < Confidence::Medium {
                "Low intelligence confidence".to_string()
            } else {
                "Some verification checks did not pass".to_string()
            };
            Verdict::ReviewRequired { reason, evidence }
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Verdict::AutoApply { .. } => "AUTO-APPLY",
            Verdict::ReviewRequired { .. } => "REVIEW-REQUIRED",
            Verdict::Escalate { .. } => "ESCALATE",
        }
    }

    pub fn emoji(&self) -> &str {
        match self {
            Verdict::AutoApply { .. } => "✅",
            Verdict::ReviewRequired { .. } => "⚠️",
            Verdict::Escalate { .. } => "🚨",
        }
    }
}

// ─── Pretty Printing ────────────────────────────────────────────────

impl fmt::Display for EvidenceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\n🦅 crow — Evidence Report")?;
        writeln!(f, "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")?;

        // Recon
        writeln!(f, "\n[1/5] Workspace Recon")?;
        writeln!(f, "  Language: {} ({})", self.recon.language, self.recon.tier)?;
        writeln!(f, "  Snapshot: {} (anchored)", self.recon.snapshot_id.0)?;
        writeln!(f, "  Files scanned: {}", self.recon.files_scanned)?;
        if !self.recon.manifests.is_empty() {
            writeln!(f, "  Manifests: {}", self.recon.manifests.join(", "))?;
        }

        // Compilation
        writeln!(f, "\n[2/5] Intent Compilation")?;
        writeln!(f, "  Rationale: \"{}\"", truncate_str(&self.compilation.rationale, 60))?;
        writeln!(f, "  Confidence: {:?}", self.compilation.confidence)?;
        writeln!(
            f,
            "  Operations: {} Modify, {} Create, {} Delete, {} Rename",
            self.compilation.modify_count,
            self.compilation.create_count,
            self.compilation.delete_count,
            self.compilation.rename_count,
        )?;

        // Hydration
        writeln!(f, "\n[3/5] Plan Hydration")?;
        if self.hydration.snapshot_verified {
            writeln!(f, "  ✅ base_snapshot_id verified against workspace")?;
        } else {
            writeln!(f, "  ❌ base_snapshot_id MISMATCH — plan may be stale")?;
        }
        writeln!(
            f,
            "  ✅ {}/{} precondition hashes match",
            self.hydration.hashes_matched, self.hydration.hashes_total
        )?;
        for warning in &self.hydration.drift_warnings {
            writeln!(f, "  ⚠️  {}", warning)?;
        }

        // Preflight
        writeln!(f, "\n[4/5] Preflight Verification ({})", self.preflight.language)?;
        match &self.preflight.outcome {
            PreflightOutcome::Clean { duration_secs } => {
                writeln!(f, "  ✅ Passed in {:.1}s (0 errors, 0 warnings)", duration_secs)?;
            }
            PreflightOutcome::Errors { count, summary } => {
                writeln!(f, "  ❌ {} error(s) detected", count)?;
                for line in summary.lines().take(5) {
                    writeln!(f, "     {}", line)?;
                }
            }
            PreflightOutcome::Skipped { reason } => {
                writeln!(f, "  ⏭️  Skipped: {}", reason)?;
            }
        }

        // Verdict
        writeln!(f, "\n[5/5] Evidence Summary")?;
        writeln!(f, "  ┌─────────────────────────────────────────────┐")?;
        writeln!(f, "  │  Verdict: {} {}",
            self.verdict.label(),
            self.verdict.emoji(),
        )?;

        let evidence = match &self.verdict {
            Verdict::AutoApply { evidence } => evidence,
            Verdict::ReviewRequired { evidence, .. } => evidence,
            Verdict::Escalate { evidence, .. } => evidence,
        };
        let compile_ok = !evidence.compile_runs.is_empty()
            && evidence.compile_runs.iter().all(|r| r.outcome == crow_evidence::types::TestOutcome::Passed);
        let lint_ok = evidence.lints_clean;
        let risk_label = if evidence.risk_flags.is_empty() { "Low" } else { "Present" };

        writeln!(
            f,
            "  │  Compile: {}  Lint: {}  Risk: {}",
            if compile_ok { "✅" } else { "❌" },
            if lint_ok { "✅" } else { "⚠️" },
            risk_label,
        )?;

        if let Verdict::ReviewRequired { reason, .. } = &self.verdict {
            writeln!(f, "  │  Reason: {}", reason)?;
        }
        if let Verdict::Escalate { risk_flags, .. } = &self.verdict {
            for flag in risk_flags {
                writeln!(f, "  │  🚨 {}", flag)?;
            }
        }

        writeln!(f, "  └─────────────────────────────────────────────┘")?;
        writeln!(f, "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")?;

        Ok(())
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crow_evidence::types::{RiskFlag, TestOutcome, TestRun, TestScope};

    fn make_green_evidence() -> EvidenceMatrix {
        EvidenceMatrix {
            compile_runs: vec![TestRun {
                command: "cargo test".into(),
                outcome: TestOutcome::Passed,
                passed: 42,
                failed: 0,
                skipped: 0,
                duration: std::time::Duration::from_millis(2100),
                truncated_log: "All tests passed".into(),
            }],
            test_scope: Some(TestScope::Full),
            has_known_baseline: true,
            lints_clean: true,
            intelligence_confidence: Confidence::High,
            risk_flags: vec![],
        }
    }

    fn make_risky_evidence() -> EvidenceMatrix {
        let mut e = make_green_evidence();
        e.risk_flags.push(RiskFlag {
            kind: RiskKind::Security,
            description: "Removed authentication check from login handler".into(),
        });
        e
    }

    #[test]
    fn verdict_auto_apply_when_green() {
        let evidence = make_green_evidence();
        let verdict = Verdict::from_evidence(evidence);
        assert!(matches!(verdict, Verdict::AutoApply { .. }));
        assert_eq!(verdict.label(), "AUTO-APPLY");
    }

    #[test]
    fn verdict_escalate_on_security_risk() {
        let evidence = make_risky_evidence();
        let verdict = Verdict::from_evidence(evidence);
        assert!(matches!(verdict, Verdict::Escalate { .. }));
        assert_eq!(verdict.label(), "ESCALATE");
    }

    #[test]
    fn verdict_review_on_no_runs() {
        let evidence = EvidenceMatrix::empty();
        let verdict = Verdict::from_evidence(evidence);
        assert!(matches!(verdict, Verdict::ReviewRequired { .. }));
    }

    #[test]
    fn compilation_summary_from_plan() {
        let plan = IntentPlan {
            base_snapshot_id: SnapshotId("test".into()),
            rationale: "Fix the auth bug".into(),
            is_partial: false,
            confidence: Confidence::High,
            operations: vec![
                EditOp::Create {
                    path: crow_patch::WorkspacePath::new("new.rs").unwrap(),
                    content: "fn main() {}".into(),
                    precondition: crow_patch::FilePrecondition::MustNotExist,
                },
                EditOp::Modify {
                    path: crow_patch::WorkspacePath::new("old.rs").unwrap(),
                    preconditions: crow_patch::PreconditionState {
                        content_hash: "abc".into(),
                        expected_line_count: Some(10),
                    },
                    hunks: vec![],
                },
            ],
        };
        let summary = CompilationSummary::from_plan(&plan);
        assert_eq!(summary.create_count, 1);
        assert_eq!(summary.modify_count, 1);
        assert_eq!(summary.delete_count, 0);
        assert_eq!(summary.total_ops(), 2);
    }

    #[test]
    fn evidence_report_display_renders() {
        let report = EvidenceReport {
            recon: ReconSummary {
                language: "rust".into(),
                tier: "Tier-1".into(),
                snapshot_id: SnapshotId("snap_a3f7c2d".into()),
                files_scanned: 47,
                manifests: vec!["Cargo.toml".into(), ".gitignore".into()],
            },
            compilation: CompilationSummary {
                rationale: "Add Result<> return types to auth functions".into(),
                confidence: Confidence::High,
                modify_count: 2,
                create_count: 0,
                delete_count: 0,
                rename_count: 0,
            },
            hydration: HydrationSummary {
                snapshot_verified: true,
                hashes_matched: 2,
                hashes_total: 2,
                drift_warnings: vec![],
            },
            preflight: PreflightSummary {
                language: "rust".into(),
                outcome: PreflightOutcome::Clean { duration_secs: 2.1 },
            },
            verdict: Verdict::from_evidence(make_green_evidence()),
        };

        let output = format!("{}", report);
        assert!(output.contains("Evidence Report"));
        assert!(output.contains("AUTO-APPLY"));
        assert!(output.contains("snap_a3f7c2d"));
        assert!(output.contains("rust"));
        assert!(output.contains("2 Modify"));
    }
}
