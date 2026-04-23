//! Command registry and parsing for Crow CLI.
//!
//! This crate provides the central registry for slash commands, autocomplete suggestions,
//! and routing logic, decoupling the TUI presentation from command definitions.

/// Retrieves autocomplete suggestions for the command palette.
pub fn get_palette_commands(query: &str) -> Vec<(String, String)> {
    let all = vec![
        ("/help", "Show manual"),
        ("/status", "Print system status"),
        ("/clear", "Clear conversation and start fresh session"),
        ("/model", "Switch LLM Model"),
        ("/view", "Swap Lens Mode (focus|evidence|audit)"),
        ("/swarm", "Launch background sub-agent swarm"),
        ("/compact", "Force context compaction"),
        ("/memory", "Manage persistent workspace memory"),
        ("/session list", "List saved sessions"),
        ("/session resume", "Resume a saved session"),
        ("/exit", "Exit Crow"),
    ];
    let trimmed_query = query.trim_end();
    if trimmed_query == "/" || trimmed_query.is_empty() {
        all.into_iter()
            .map(|(c, d)| (c.to_string(), d.to_string()))
            .collect()
    } else {
        all.into_iter()
            .filter(|(cmd, _)| cmd.starts_with(trimmed_query))
            .map(|(c, d)| (c.to_string(), d.to_string()))
            .collect()
    }
}
