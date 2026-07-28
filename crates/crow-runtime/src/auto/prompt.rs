use super::AutoAgentSpec;
use super::AutoPhaseKind;

pub fn render_node_prompt(spec: &AutoAgentSpec, handoff_context: &str) -> String {
    let phase_instruction = match spec.phase {
        AutoPhaseKind::Explore => {
            "Read-only exploration. Return concrete files, reusable patterns, and risks."
        }
        AutoPhaseKind::Plan => {
            "Read-only planning. Return a phased implementation plan and verification strategy."
        }
        AutoPhaseKind::Execute => {
            "Implement the approved plan safely. Keep changes focused and reuse existing code."
        }
        AutoPhaseKind::Review => {
            "Review the implementation for correctness, simplification, security, and test gaps. Final line format: RESULT: pass | fail | blocked — <one sentence reason>."
        }
        AutoPhaseKind::Verify => {
            "Run or recommend targeted verification and summarize pass/fail evidence. Final line format: RESULT: pass | fail | blocked — <one sentence reason>."
        }
    };

    format!(
        "{phase_instruction}\n\nTask prompt:\n{task}\n\nPrior artifacts:\n{handoff_context}",
        task = spec.prompt
    )
}
