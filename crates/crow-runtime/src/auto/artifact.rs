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
    fn renders_handoff_context_with_prior_artifacts() {
        let mut store = ArtifactStore::default();
        store.push(AgentArtifactBundle {
            node_id: AutoNodeId("planner-1".into()),
            phase: AutoPhaseKind::Plan,
            final_text: "Plan the coordinator first".into(),
            summaries: vec![ArtifactSummary {
                kind: ArtifactKind::Plan,
                title: "Coordinator".into(),
                preview: "Introduce AutoRunCoordinator".into(),
            }],
            files_read: Vec::new(),
            files_changed: Vec::new(),
            verification_commands: Vec::new(),
            success: true,
        });

        let rendered = store.render_handoff_context();

        assert!(rendered.contains("Plan"));
        assert!(rendered.contains("Introduce AutoRunCoordinator"));
    }
}
