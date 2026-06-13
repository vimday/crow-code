//! Command registry and parsing for Crow CLI.
//!
//! This crate provides the central registry for slash commands, autocomplete suggestions,
//! and routing logic, decoupling the TUI presentation from command definitions.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

/// One slash command's metadata. Used both by the TUI palette and `/help`.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub trigger: &'static str,
    pub category: Category,
    pub description: &'static str,
    pub usage_hint: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Session,
    Context,
    View,
    Workspace,
    System,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Session => "Session",
            Category::Context => "Context",
            Category::View => "View",
            Category::Workspace => "Workspace",
            Category::System => "System",
        }
    }
}

/// The full command catalog. Single source of truth for command metadata.
pub fn catalog() -> &'static [CommandSpec] {
    &[
        CommandSpec {
            trigger: "/help",
            category: Category::System,
            description: "Show the manual and keyboard shortcuts",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/status",
            category: Category::System,
            description: "Print system status, workspace info, and provider health",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/clear",
            category: Category::Session,
            description: "Clear conversation and start a fresh session",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/model",
            category: Category::Session,
            description: "Switch the active LLM model",
            usage_hint: Some("/model <provider|alias>"),
        },
        CommandSpec {
            trigger: "/view",
            category: Category::View,
            description: "Swap the lens mode (focus | evidence | audit)",
            usage_hint: Some("/view focus | evidence | audit"),
        },
        CommandSpec {
            trigger: "/swarm",
            category: Category::Session,
            description: "Launch a background sub-agent swarm",
            usage_hint: Some("/swarm <task description>"),
        },
        CommandSpec {
            trigger: "/compact",
            category: Category::Context,
            description: "Force context compaction now",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/diff",
            category: Category::Workspace,
            description: "Show the workspace diff (including untracked files)",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/undo",
            category: Category::Workspace,
            description: "Revert file changes from the last agent turn",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/tokens",
            category: Category::Context,
            description: "Show current context window usage",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/cost",
            category: Category::Context,
            description: "Show token usage and estimated cost summary",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/memory",
            category: Category::Context,
            description: "Manage persistent workspace memory",
            usage_hint: Some("/memory list | add | remove"),
        },
        CommandSpec {
            trigger: "/session list",
            category: Category::Session,
            description: "List saved sessions",
            usage_hint: None,
        },
        CommandSpec {
            trigger: "/session resume",
            category: Category::Session,
            description: "Resume a saved session",
            usage_hint: Some("/session resume <id>"),
        },
        CommandSpec {
            trigger: "/exit",
            category: Category::System,
            description: "Exit Crow",
            usage_hint: None,
        },
    ]
}

/// Retrieves autocomplete suggestions for the command palette.
/// Returns (trigger, description) pairs. Kept for backwards compatibility
/// with the existing TUI; new code should use `catalog()` directly.
pub fn get_palette_commands(query: &str) -> Vec<(String, String)> {
    let trimmed = query.trim_end();
    let prefix = if trimmed == "/" || trimmed.is_empty() {
        ""
    } else {
        trimmed
    };

    catalog()
        .iter()
        .filter(|spec| spec.trigger.starts_with(prefix))
        .map(|spec| {
            let desc = if let Some(hint) = spec.usage_hint {
                format!("{} · {}", spec.description, hint)
            } else {
                spec.description.to_string()
            };
            (spec.trigger.to_string(), desc)
        })
        .collect()
}

/// Look up a command by its trigger. Returns the spec for `/help`-style introspection.
pub fn find(trigger: &str) -> Option<&'static CommandSpec> {
    catalog().iter().find(|spec| spec.trigger == trigger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all() {
        let suggestions = get_palette_commands("");
        assert_eq!(suggestions.len(), catalog().len());
    }

    #[test]
    fn prefix_filter_works() {
        let suggestions = get_palette_commands("/sess");
        assert!(suggestions.iter().all(|(t, _)| t.starts_with("/sess")));
        assert!(suggestions.len() >= 2);
    }

    #[test]
    fn descriptions_render_usage_hints() {
        let suggestions = get_palette_commands("/model");
        let model = suggestions
            .iter()
            .find(|(t, _)| t == "/model")
            .expect("/model should always be in the catalog");
        assert!(model.1.contains("·"));
        assert!(model.1.contains("provider"));
    }
}
