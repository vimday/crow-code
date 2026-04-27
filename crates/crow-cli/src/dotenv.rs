//! Dotenv file parser and credential discovery.
//!
//! Provides workspace-local `.env` file loading for API key discovery.
//! Ported from claw-code's `providers/mod.rs::parse_dotenv()`.
//!
//! ## Priority Chain
//!
//! 1. Process environment variables (`std::env::var`)
//! 2. Workspace `.env` file (this module)
//! 3. Config file (`~/.crow/config.json` or `.crow/config.json`)
//! 4. Error with provider-specific hints

use std::collections::HashMap;
use std::path::Path;

/// Parse a `.env` file body into key/value pairs.
///
/// Supports:
/// - `KEY=VALUE` pairs
/// - `export KEY=VALUE` (shell-compatible)
/// - Single and double quoted values
/// - Comments (`#`)
/// - Empty lines
///
/// Does NOT support:
/// - Multi-line values
/// - Variable interpolation
/// - Escaped quotes within values
///
/// Ported from claw-code's `parse_dotenv()`.
pub fn parse_dotenv(content: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let trimmed_key = raw_key.trim();
        let key = trimmed_key
            .strip_prefix("export ")
            .map_or(trimmed_key, str::trim)
            .to_string();
        if key.is_empty() {
            continue;
        }
        let trimmed_value = raw_value.trim();
        let unquoted = if (trimmed_value.starts_with('"') && trimmed_value.ends_with('"')
            || trimmed_value.starts_with('\'') && trimmed_value.ends_with('\''))
            && trimmed_value.len() >= 2
        {
            &trimmed_value[1..trimmed_value.len() - 1]
        } else {
            trimmed_value
        };
        values.insert(key, unquoted.to_string());
    }
    values
}

/// Load and parse a `.env` file from the given path.
///
/// Returns `None` if the file doesn't exist or can't be read.
pub fn load_dotenv_file(path: &Path) -> Option<HashMap<String, String>> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse_dotenv(&content))
}

/// Look up a key in the workspace `.env` file.
///
/// Searches for `.env` in the current working directory.
/// Returns `None` if the file is missing, the key is absent, or the value is empty.
pub fn dotenv_value(key: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    dotenv_value_from(&cwd, key)
}

/// Look up a key in a `.env` file relative to a specific directory.
pub fn dotenv_value_from(dir: &Path, key: &str) -> Option<String> {
    let values = load_dotenv_file(&dir.join(".env"))?;
    values.get(key).filter(|v| !v.is_empty()).cloned()
}

/// Check whether an env var is set (non-empty) in either the process
/// environment or the workspace `.env` file.
///
/// This mirrors claw-code's `env_or_dotenv_present()`.
pub fn env_or_dotenv_present(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => true,
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            dotenv_value(key).is_some_and(|v| !v.is_empty())
        }
        Err(_) => false,
    }
}

/// Resolve an env var from either the process environment or `.env` file.
///
/// Process environment takes priority over `.env`.
pub fn resolve_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dotenv_value(key))
}

/// Resolve an env var from process or a specific directory's `.env` file.
pub fn resolve_env_from(dir: &Path, key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| dotenv_value_from(dir, key))
}

// ─── Foreign Provider Credential Hints ──────────────────────────────

/// Known foreign provider credentials and their routing hints.
///
/// When the user configures provider A but only has credentials for
/// provider B, we detect this and suggest the correct fix.
const FOREIGN_PROVIDER_HINTS: &[(&str, &str, &str)] = &[
    (
        "OPENAI_API_KEY",
        "OpenAI-compatible",
        "Set LLM_PROVIDER=openai or use a model like 'gpt-4o'",
    ),
    (
        "ANTHROPIC_API_KEY",
        "Anthropic",
        "Set LLM_PROVIDER=anthropic or use a model like 'sonnet'",
    ),
    (
        "DEEPSEEK_API_KEY",
        "DeepSeek",
        "Set LLM_PROVIDER=deepseek or use model 'deepseek-chat'",
    ),
    (
        "KIMI_API_KEY",
        "Kimi/Moonshot",
        "Set LLM_PROVIDER=kimi or use a model like 'kimi'",
    ),
    (
        "MOONSHOT_API_KEY",
        "Kimi/Moonshot",
        "Set LLM_PROVIDER=kimi or use a model like 'kimi'",
    ),
    (
        "XAI_API_KEY",
        "xAI",
        "Set LLM_PROVIDER=xai or use a model like 'grok'",
    ),
    (
        "DASHSCOPE_API_KEY",
        "Alibaba DashScope",
        "Set LLM_PROVIDER=qwen or use a model like 'qwen-max'",
    ),
    (
        "GLM_API_KEY",
        "Zhipu AI",
        "Set LLM_PROVIDER=glm or use a model like 'glm-4-plus'",
    ),
];

/// Generate a hint string when a provider's credentials are missing
/// but a *different* provider's credentials are available.
///
/// Returns `None` when no foreign credentials are found.
///
/// Ported from claw-code's `anthropic_missing_credentials_hint()`.
pub fn missing_credentials_hint(current_provider: &str) -> Option<String> {
    let current_lower = current_provider.to_lowercase();
    for (env_var, provider_label, fix_hint) in FOREIGN_PROVIDER_HINTS {
        // Skip the provider we're already trying to use
        if provider_label.to_lowercase().contains(&current_lower) {
            continue;
        }
        if env_or_dotenv_present(env_var) {
            return Some(format!(
                "I see {env_var} is set — if you meant to use the {provider_label} provider, {fix_hint}."
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_dotenv() {
        let content = "KEY1=value1\nKEY2=value2\n";
        let parsed = parse_dotenv(content);
        assert_eq!(parsed.get("KEY1").map(String::as_str), Some("value1"));
        assert_eq!(parsed.get("KEY2").map(String::as_str), Some("value2"));
    }

    #[test]
    fn handles_quotes() {
        let content = r#"
SINGLE='single quoted'
DOUBLE="double quoted"
BARE=no quotes
"#;
        let parsed = parse_dotenv(content);
        assert_eq!(
            parsed.get("SINGLE").map(String::as_str),
            Some("single quoted")
        );
        assert_eq!(
            parsed.get("DOUBLE").map(String::as_str),
            Some("double quoted")
        );
        assert_eq!(parsed.get("BARE").map(String::as_str), Some("no quotes"));
    }

    #[test]
    fn handles_export_prefix() {
        let content = "export API_KEY=sk-12345\n";
        let parsed = parse_dotenv(content);
        assert_eq!(parsed.get("API_KEY").map(String::as_str), Some("sk-12345"));
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let content = "# comment\n\nKEY=value\n  # another comment\n";
        let parsed = parse_dotenv(content);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("KEY").map(String::as_str), Some("value"));
    }

    #[test]
    fn skips_lines_without_equals() {
        let content = "VALID=yes\nINVALID_LINE\n";
        let parsed = parse_dotenv(content);
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn handles_empty_value() {
        let content = "EMPTY_KEY=\n";
        let parsed = parse_dotenv(content);
        assert_eq!(parsed.get("EMPTY_KEY").map(String::as_str), Some(""));
    }

    #[test]
    fn handles_value_with_equals() {
        let content = "URL=https://api.example.com?key=value\n";
        let parsed = parse_dotenv(content);
        assert_eq!(
            parsed.get("URL").map(String::as_str),
            Some("https://api.example.com?key=value")
        );
    }

    #[test]
    fn load_nonexistent_file_returns_none() {
        assert!(load_dotenv_file(Path::new("/nonexistent/.env")).is_none());
    }
}
