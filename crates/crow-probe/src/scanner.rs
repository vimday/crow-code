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
        // Only promote primary_lang if nothing higher-tier was detected.
        // Tier ordering: Tier3 < Tier2 < Tier1, so "< Tier2" means unknown/Tier3.
        if primary_lang.tier < LanguageTier::Tier2 {
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
    candidates.sort_by_key(|b| std::cmp::Reverse(b.confidence));

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn node_only_repo_detects_javascript() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();

        let profile = scan_workspace(dir.path()).unwrap();
        assert_eq!(profile.primary_lang.name, "javascript");
        assert_eq!(profile.primary_lang.tier, LanguageTier::Tier2);
        assert_eq!(profile.verification_candidates.len(), 1);
        assert!(profile
            .ignore_spec
            .artifact_dirs
            .contains(&"node_modules".to_string()));
    }

    #[test]
    fn ts_only_repo_detects_typescript() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();

        let profile = scan_workspace(dir.path()).unwrap();
        assert_eq!(profile.primary_lang.name, "typescript");
        assert_eq!(profile.primary_lang.tier, LanguageTier::Tier2);
    }

    #[test]
    fn polyglot_rust_plus_node_keeps_rust_primary() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();

        let profile = scan_workspace(dir.path()).unwrap();
        // Rust is Tier1 — must NOT be downgraded to JS/Tier2
        assert_eq!(profile.primary_lang.name, "rust");
        assert_eq!(profile.primary_lang.tier, LanguageTier::Tier1);
        // But both candidates must be emitted
        assert_eq!(profile.verification_candidates.len(), 2);
        let programs: Vec<&str> = profile
            .verification_candidates
            .iter()
            .map(|c| c.command.program.as_str())
            .collect();
        assert!(programs.contains(&"cargo"));
        assert!(programs.contains(&"npm"));
        // Both artifact dirs must be present
        assert!(profile
            .ignore_spec
            .artifact_dirs
            .contains(&"target".to_string()));
        assert!(profile
            .ignore_spec
            .artifact_dirs
            .contains(&"node_modules".to_string()));
    }

    #[test]
    fn empty_repo_returns_unknown() {
        let dir = TempDir::new().unwrap();
        let profile = scan_workspace(dir.path()).unwrap();
        assert_eq!(profile.primary_lang.name, "unknown");
        assert_eq!(profile.primary_lang.tier, LanguageTier::Tier3);
        assert!(profile.verification_candidates.is_empty());
    }

    #[test]
    fn gitignore_negation_rules_are_filtered_out() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(".gitignore"),
            "target/\n# keep this file\n!important.rs\n*.log\n",
        )
        .unwrap();

        let profile = scan_workspace(dir.path()).unwrap();
        let patterns = &profile.ignore_spec.ignore_patterns;
        // Positive patterns should be included
        assert!(patterns.contains(&"target/".to_string()));
        assert!(patterns.contains(&"*.log".to_string()));
        // Negation rules must NOT leak through — globset can't represent them
        assert!(
            !patterns.iter().any(|p| p.starts_with('!')),
            "negation rules should be filtered: {:?}",
            patterns
        );
        // Comments must also be excluded
        assert!(
            !patterns.iter().any(|p| p.starts_with('#')),
            "comments should be filtered: {:?}",
            patterns
        );
    }
}
