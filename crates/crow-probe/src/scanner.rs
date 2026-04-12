use crate::types::*;
use std::fs;
use std::path::Path;

/// Heuristically scans a workspace root to discover languages,
/// artifacts, and candidate verification commands.
///
/// Polyglot repos (e.g. Cargo.toml + package.json) emit candidates
/// for every detected toolchain. The primary language is set to the
/// highest-tier detection, with Rust > Node/TS > unknown.
pub fn scan_workspace(root: &Path) -> Result<ProjectProfile, String> {
    let mut candidates = Vec::new();
    let mut artifact_dirs = Vec::new();
    let mut ignore_patterns = vec![".git".into(), ".svn".into()];
    let mut primary_lang = DetectedLanguage {
        name: "unknown".into(),
        tier: LanguageTier::Tier3,
    };

    // 1. Rust / Cargo
    if root.join("Cargo.toml").exists() {
        primary_lang = DetectedLanguage {
            name: "rust".into(),
            tier: LanguageTier::Tier1,
        };
        artifact_dirs.push("target".into());
        candidates.push(VerificationCandidate {
            command: VerificationCommand::new("cargo", vec!["test", "--workspace"]),
            kind: VerificationKind::Test,
            confidence: ProbeConfidence::ManifestBacked,
            evidence_source: "Cargo.toml".into(),
        });
    }

    // 2. Node.js / TS — independent check, not mutually exclusive
    if root.join("package.json").exists() {
        let is_ts = root.join("tsconfig.json").exists();
        // Only promote primary_lang if nothing higher-tier was detected
        if primary_lang.tier > LanguageTier::Tier2 {
            primary_lang = DetectedLanguage {
                name: if is_ts {
                    "typescript".into()
                } else {
                    "javascript".into()
                },
                tier: LanguageTier::Tier2,
            };
        }
        artifact_dirs.push("node_modules".into());
        artifact_dirs.push("dist".into());
        candidates.push(VerificationCandidate {
            command: VerificationCommand::new("npm", vec!["test"]),
            kind: VerificationKind::Test,
            confidence: ProbeConfidence::Inferred,
            evidence_source: "package.json".into(),
        });
    }

    // 3. Extract .gitignore (filter out negation rules and comments
    //    that globset cannot represent — logs a silent skip for now)
    if let Ok(content) = fs::read_to_string(root.join(".gitignore")) {
        for line in content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            // Skip negation/re-inclusion rules (e.g. "!keep.rs")
            // that have no equivalent in globset semantics.
            if line.starts_with('!') {
                continue;
            }
            ignore_patterns.push(line.to_string());
        }
    }

    // Sort descending by confidence so highest confidence runs first
    candidates.sort_by(|a, b| b.confidence.cmp(&a.confidence));

    Ok(ProjectProfile {
        primary_lang,
        workspace_root: root.to_path_buf(),
        verification_candidates: candidates,
        ignore_spec: FilterSpec {
            ignore_patterns,
            artifact_dirs,
        },
    })
}
