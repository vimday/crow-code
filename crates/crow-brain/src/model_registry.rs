//! Model registry: token limits, alias resolution, and context preflight.
//!
//! Ported from claw-code's `providers/mod.rs`. Provides:
//! - Known model token limits (context window + max output tokens)
//! - Model alias resolution (e.g., "sonnet" → "claude-sonnet-4-6")
//! - Pre-flight context window validation (catch overflows before API calls)

use crate::ChatMessage;

// ─── Token Limits ───────────────────────────────────────────────────

/// Token limit metadata for a known model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTokenLimit {
    /// Maximum output tokens the model can generate in one response.
    pub max_output_tokens: u32,
    /// Total context window size in tokens.
    pub context_window_tokens: u32,
}

/// Look up token limits for a known model.
///
/// Returns `None` for unknown/custom models (e.g., Ollama local models).
/// The caller should fall back to conservative defaults in that case.
#[must_use]
pub fn model_token_limit(model: &str) -> Option<ModelTokenLimit> {
    let canonical = resolve_model_alias(model);
    match canonical.as_str() {
        // ── Anthropic ───────────────────────────────────────────
        "claude-opus-4-6" => Some(ModelTokenLimit {
            max_output_tokens: 32_000,
            context_window_tokens: 200_000,
        }),
        "claude-sonnet-4-6" | "claude-haiku-4-5-20251213" => Some(ModelTokenLimit {
            max_output_tokens: 64_000,
            context_window_tokens: 200_000,
        }),
        // Older Claude models
        m if m.starts_with("claude-3-7-sonnet") || m.starts_with("claude-3-5-sonnet") => {
            Some(ModelTokenLimit {
                max_output_tokens: 8_192,
                context_window_tokens: 200_000,
            })
        }
        m if m.starts_with("claude-3-5-haiku") => Some(ModelTokenLimit {
            max_output_tokens: 8_192,
            context_window_tokens: 200_000,
        }),

        // ── OpenAI ──────────────────────────────────────────────
        "gpt-4o" | "gpt-4o-2024-08-06" | "gpt-4o-2024-11-20" => Some(ModelTokenLimit {
            max_output_tokens: 16_384,
            context_window_tokens: 128_000,
        }),
        "gpt-4o-mini" | "gpt-4o-mini-2024-07-18" => Some(ModelTokenLimit {
            max_output_tokens: 16_384,
            context_window_tokens: 128_000,
        }),
        "gpt-4-turbo" | "gpt-4-turbo-2024-04-09" => Some(ModelTokenLimit {
            max_output_tokens: 4_096,
            context_window_tokens: 128_000,
        }),
        m if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") => {
            Some(ModelTokenLimit {
                max_output_tokens: 32_000,
                context_window_tokens: 200_000,
            })
        }

        // ── xAI (Grok) ─────────────────────────────────────────
        "grok-3" | "grok-3-mini" => Some(ModelTokenLimit {
            max_output_tokens: 64_000,
            context_window_tokens: 131_072,
        }),
        "grok-2" | "grok-2-latest" => Some(ModelTokenLimit {
            max_output_tokens: 32_000,
            context_window_tokens: 131_072,
        }),

        // ── DeepSeek ────────────────────────────────────────────
        "deepseek-chat" | "deepseek-coder" | "deepseek-reasoner" => Some(ModelTokenLimit {
            max_output_tokens: 8_192,
            context_window_tokens: 128_000,
        }),

        // ── Kimi (Moonshot AI via DashScope) ────────────────────
        "kimi-k2.5" | "kimi-k1.5" => Some(ModelTokenLimit {
            max_output_tokens: 16_384,
            context_window_tokens: 256_000,
        }),
        "moonshot-v1-auto" | "moonshot-v1-128k" => Some(ModelTokenLimit {
            max_output_tokens: 8_192,
            context_window_tokens: 128_000,
        }),

        // ── Qwen (Alibaba DashScope) ────────────────────────────
        "qwen-max" | "qwen-plus" | "qwen-turbo" => Some(ModelTokenLimit {
            max_output_tokens: 8_192,
            context_window_tokens: 128_000,
        }),

        // ── GLM (Zhipu AI) ──────────────────────────────────────
        "glm-4-plus" | "glm-4" => Some(ModelTokenLimit {
            max_output_tokens: 4_096,
            context_window_tokens: 128_000,
        }),

        _ => None,
    }
}

/// Get the max output tokens for a model, with sensible defaults for unknown models.
#[must_use]
pub fn max_tokens_for_model(model: &str) -> u32 {
    model_token_limit(model).map_or_else(
        || {
            let canonical = resolve_model_alias(model);
            if canonical.contains("opus") {
                32_000
            } else {
                8_192 // Conservative default
            }
        },
        |limit| limit.max_output_tokens,
    )
}

/// Returns the effective max output tokens, preferring an explicit override.
#[must_use]
pub fn max_tokens_with_override(model: &str, override_val: Option<u32>) -> u32 {
    override_val.unwrap_or_else(|| max_tokens_for_model(model))
}

// ─── Model Alias Resolution ────────────────────────────────────────

/// Resolve a model alias to its canonical model identifier.
///
/// Allows users to type "sonnet" instead of "claude-sonnet-4-6", or
/// "grok" instead of "grok-3". Unknown aliases are returned as-is.
#[must_use]
pub fn resolve_model_alias(model: &str) -> String {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();

    match lower.as_str() {
        // Anthropic aliases
        "opus" => "claude-opus-4-6".to_string(),
        "sonnet" => "claude-sonnet-4-6".to_string(),
        "haiku" => "claude-haiku-4-5-20251213".to_string(),

        // xAI aliases
        "grok" | "grok-3" => "grok-3".to_string(),
        "grok-mini" | "grok-3-mini" => "grok-3-mini".to_string(),
        "grok-2" => "grok-2".to_string(),

        // Kimi alias
        "kimi" => "kimi-k2.5".to_string(),

        // DeepSeek aliases
        "deepseek" => "deepseek-chat".to_string(),

        // Qwen aliases
        "qwen" => "qwen-max".to_string(),

        // GLM aliases
        "glm" => "glm-4-plus".to_string(),

        // No alias — return as-is
        _ => trimmed.to_string(),
    }
}

// ─── Provider Detection from Model Name ─────────────────────────────

/// Detect the LLM provider from a model name.
///
/// Returns a provider string suitable for use in config resolution.
/// Returns `None` if the model name doesn't match any known provider prefix.
#[must_use]
pub fn detect_provider_from_model(model: &str) -> Option<&'static str> {
    let canonical = resolve_model_alias(model);

    if canonical.starts_with("claude") {
        return Some("anthropic");
    }
    if canonical.starts_with("gpt-")
        || canonical.starts_with("o1")
        || canonical.starts_with("o3")
        || canonical.starts_with("o4")
        || canonical.starts_with("gpt-5")
        || canonical.starts_with("openai/")
    {
        return Some("openai");
    }
    if canonical.starts_with("grok") {
        return Some("xai");
    }
    if canonical.starts_with("deepseek") {
        return Some("deepseek");
    }
    if canonical.starts_with("kimi") || canonical.starts_with("moonshot") {
        return Some("kimi");
    }
    if canonical.starts_with("qwen") {
        return Some("qwen");
    }
    if canonical.starts_with("glm") {
        return Some("glm");
    }

    None
}

// ─── Context Preflight ──────────────────────────────────────────────

/// Proactively check if a message set will fit in the model's context window.
///
/// This is the crow-code equivalent of claw-code's `preflight_message_request()`.
/// Catches context-window overflows BEFORE making an API call, saving latency
/// and API credits.
///
/// Returns `Ok(estimated_tokens)` if the request fits, or `Err` with details.
pub fn preflight_context_check(
    messages: &[ChatMessage],
    model: &str,
    max_output_tokens: u32,
) -> Result<u32, String> {
    let Some(limit) = model_token_limit(model) else {
        // Unknown model — skip preflight (let the API handle it)
        return Ok(0);
    };

    let estimated_input_tokens = estimate_message_tokens(messages);
    let estimated_total = estimated_input_tokens.saturating_add(max_output_tokens);

    if estimated_total > limit.context_window_tokens {
        return Err(format!(
            "Context window exceeded for model '{model}': \
             estimated {estimated_input_tokens} input tokens + {max_output_tokens} output tokens \
             = {estimated_total} total, but context window is {} tokens. \
             Consider compacting the conversation or reducing input size.",
            limit.context_window_tokens
        ));
    }

    Ok(estimated_input_tokens)
}

/// Estimate token count for a set of messages using the ~4 chars/token heuristic.
fn estimate_message_tokens(messages: &[ChatMessage]) -> u32 {
    let total_chars: usize = messages
        .iter()
        .map(|m| {
            let mut chars = m.content.len();
            // Account for tool call arguments
            if let Some(ref tcs) = m.tool_calls {
                for tc in tcs {
                    chars += tc.arguments.to_string().len();
                    chars += tc.name.len();
                }
            }
            chars
        })
        .sum();

    // ~4 chars per token, with 10% overhead for message framing
    let base_tokens = total_chars / 4;
    let framing_overhead = messages.len() * 4; // ~4 tokens per message boundary
    (base_tokens + framing_overhead) as u32
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resolves_aliases() {
        assert_eq!(resolve_model_alias("sonnet"), "claude-sonnet-4-6");
        assert_eq!(resolve_model_alias("opus"), "claude-opus-4-6");
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
        assert_eq!(resolve_model_alias("kimi"), "kimi-k2.5");
        assert_eq!(resolve_model_alias("deepseek"), "deepseek-chat");
        assert_eq!(resolve_model_alias("SONNET"), "claude-sonnet-4-6"); // case insensitive
    }

    #[test]
    fn passes_through_unknown_aliases() {
        assert_eq!(resolve_model_alias("my-custom-model"), "my-custom-model");
        assert_eq!(resolve_model_alias("llama3"), "llama3");
    }

    #[test]
    fn returns_token_limits_for_known_models() {
        let limit =
            model_token_limit("claude-sonnet-4-6").expect("claude-sonnet-4-6 should have limits");
        assert_eq!(limit.context_window_tokens, 200_000);
        assert_eq!(limit.max_output_tokens, 64_000);

        let limit = model_token_limit("grok-3").expect("grok-3 should have limits");
        assert_eq!(limit.context_window_tokens, 131_072);

        let limit = model_token_limit("kimi-k2.5").expect("kimi-k2.5 should have limits");
        assert_eq!(limit.context_window_tokens, 256_000);
    }

    #[test]
    fn alias_resolves_to_correct_limits() {
        let alias_limit = model_token_limit("sonnet").expect("sonnet alias should resolve");
        let direct_limit = model_token_limit("claude-sonnet-4-6").expect("direct should resolve");
        assert_eq!(
            alias_limit.max_output_tokens,
            direct_limit.max_output_tokens
        );
    }

    #[test]
    fn returns_none_for_unknown_models() {
        assert!(model_token_limit("my-custom-model").is_none());
        assert!(model_token_limit("llama3:8b").is_none());
    }

    #[test]
    fn max_tokens_falls_back_for_unknown() {
        assert_eq!(max_tokens_for_model("unknown-model"), 8_192);
        assert_eq!(max_tokens_for_model("claude-opus-4-6"), 32_000);
    }

    #[test]
    fn override_takes_precedence() {
        assert_eq!(
            max_tokens_with_override("claude-opus-4-6", Some(12345)),
            12345
        );
        assert_eq!(max_tokens_with_override("claude-opus-4-6", None), 32_000);
    }

    #[test]
    fn detects_provider_from_model_name() {
        assert_eq!(
            detect_provider_from_model("claude-sonnet-4-6"),
            Some("anthropic")
        );
        assert_eq!(detect_provider_from_model("sonnet"), Some("anthropic"));
        assert_eq!(detect_provider_from_model("gpt-4o"), Some("openai"));
        assert_eq!(detect_provider_from_model("grok"), Some("xai"));
        assert_eq!(detect_provider_from_model("deepseek"), Some("deepseek"));
        assert_eq!(detect_provider_from_model("kimi"), Some("kimi"));
        assert_eq!(detect_provider_from_model("qwen-max"), Some("qwen"));
        assert_eq!(detect_provider_from_model("unknown"), None);
    }

    #[test]
    fn preflight_passes_small_context() {
        let msgs = vec![ChatMessage::user("Hello!")];
        let result = preflight_context_check(&msgs, "claude-sonnet-4-6", 8192);
        assert!(result.is_ok());
    }

    #[test]
    fn preflight_catches_overflow() {
        // Create a message that's way too big
        let huge_msg = "x".repeat(900_000); // ~225K tokens
        let msgs = vec![ChatMessage::user(&huge_msg)];
        let result = preflight_context_check(&msgs, "claude-sonnet-4-6", 64_000);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exceeded"));
    }

    #[test]
    fn preflight_skips_unknown_models() {
        let huge_msg = "x".repeat(900_000);
        let msgs = vec![ChatMessage::user(&huge_msg)];
        // Unknown models skip preflight
        let result = preflight_context_check(&msgs, "my-custom-model", 64_000);
        assert!(result.is_ok());
    }
}
