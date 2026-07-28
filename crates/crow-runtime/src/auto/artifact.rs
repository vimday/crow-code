use super::graph::AutoNodeId;
use super::AutoPhaseKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Exploration,
    Plan,
    Implementation,
    Review,
    Verification,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub node_id: AutoNodeId,
    pub kind: ArtifactKind,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub kind: ArtifactKind,
    pub title: String,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentArtifactBundle {
    pub node_id: AutoNodeId,
    pub phase: AutoPhaseKind,
    pub final_text: String,
    pub summaries: Vec<ArtifactSummary>,
    pub files_read: Vec<String>,
    pub files_changed: Vec<String>,
    pub verification_commands: Vec<String>,
    pub success: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactRunSummary {
    pub total_artifacts: usize,
    pub successful_artifacts: usize,
    pub failed_artifacts: usize,
    pub files_read: Vec<String>,
    pub files_changed: Vec<String>,
    pub verification_commands: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactStore {
    bundles: Vec<AgentArtifactBundle>,
}

impl ArtifactStore {
    pub fn push(&mut self, bundle: AgentArtifactBundle) {
        self.bundles.push(bundle);
    }

    pub fn by_node(&self, node_id: &AutoNodeId) -> Vec<AgentArtifactBundle> {
        self.bundles
            .iter()
            .filter(|bundle| bundle.node_id == *node_id)
            .cloned()
            .collect()
    }

    pub fn all(&self) -> &[AgentArtifactBundle] {
        &self.bundles
    }

    pub fn render_handoff_context(&self) -> String {
        if self.bundles.is_empty() {
            return "No prior auto artifacts.".to_string();
        }

        self.bundles
            .iter()
            .flat_map(|bundle| {
                bundle.summaries.iter().map(move |summary| {
                    format!(
                        "[{phase:?}/{kind:?}] {title}: {preview}",
                        phase = bundle.phase,
                        kind = summary.kind,
                        title = summary.title,
                        preview = summary.preview
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn run_summary(&self) -> ArtifactRunSummary {
        let mut summary = ArtifactRunSummary {
            total_artifacts: self.bundles.len(),
            successful_artifacts: self.bundles.iter().filter(|bundle| bundle.success).count(),
            failed_artifacts: self.bundles.iter().filter(|bundle| !bundle.success).count(),
            files_read: self
                .bundles
                .iter()
                .flat_map(|bundle| bundle.files_read.iter().cloned())
                .collect(),
            files_changed: self
                .bundles
                .iter()
                .flat_map(|bundle| bundle.files_changed.iter().cloned())
                .collect(),
            verification_commands: self
                .bundles
                .iter()
                .flat_map(|bundle| bundle.verification_commands.iter().cloned())
                .collect(),
        };
        summary.files_read.sort();
        summary.files_read.dedup();
        summary.files_changed.sort();
        summary.files_changed.dedup();
        summary.verification_commands.sort();
        summary.verification_commands.dedup();
        summary
    }
}

pub fn bounded_preview(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut out = compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto::graph::AutoNodeId;
    use crate::auto::AutoPhaseKind;

    #[test]
    fn stores_and_lists_bundles_by_node() {
        let mut store = ArtifactStore::default();
        let node_id = AutoNodeId("explorer-1".into());
        let bundle = AgentArtifactBundle {
            node_id: node_id.clone(),
            phase: AutoPhaseKind::Explore,
            final_text: "Found runtime and TUI files".into(),
            summaries: vec![ArtifactSummary {
                kind: ArtifactKind::Exploration,
                title: "Files".into(),
                preview: "auto.rs, thread_manager.rs".into(),
            }],
            files_read: vec!["crates/crow-runtime/src/auto.rs".into()],
            files_changed: Vec::new(),
            verification_commands: Vec::new(),
            success: true,
        };

        store.push(bundle.clone());

        assert_eq!(store.by_node(&node_id), vec![bundle]);
        assert_eq!(store.all().len(), 1);
    }

    #[test]
    fn run_summary_deduplicates_artifact_metadata() {
        let mut store = ArtifactStore::default();
        store.push(AgentArtifactBundle {
            node_id: AutoNodeId("verifier-1".into()),
            phase: AutoPhaseKind::Verify,
            final_text: "cargo test passed".into(),
            summaries: Vec::new(),
            files_read: vec!["Cargo.toml".into(), "Cargo.toml".into()],
            files_changed: vec!["src/lib.rs".into()],
            verification_commands: vec!["cargo test".into(), "cargo test".into()],
            success: true,
        });
        store.push(AgentArtifactBundle {
            node_id: AutoNodeId("reviewer-1".into()),
            phase: AutoPhaseKind::Review,
            final_text: "found gap".into(),
            summaries: Vec::new(),
            files_read: vec!["src/lib.rs".into()],
            files_changed: vec!["src/lib.rs".into()],
            verification_commands: Vec::new(),
            success: false,
        });

        let summary = store.run_summary();

        assert_eq!(summary.total_artifacts, 2);
        assert_eq!(summary.successful_artifacts, 1);
        assert_eq!(summary.failed_artifacts, 1);
        assert_eq!(summary.files_read, vec!["Cargo.toml", "src/lib.rs"]);
        assert_eq!(summary.files_changed, vec!["src/lib.rs"]);
        assert_eq!(summary.verification_commands, vec!["cargo test"]);
    }

    #[test]
    fn bounded_preview_compacts_and_truncates_text() {
        let preview = bounded_preview("alpha\n beta   gamma delta", 13);

        assert_eq!(preview, "alpha beta g…");
    }
}
