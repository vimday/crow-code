#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Tokens written to Anthropic's prompt cache during this request.
    pub cache_creation_input_tokens: u32,
    /// Tokens read from Anthropic's prompt cache during this request.
    pub cache_read_input_tokens: u32,
}

impl TokenUsage {
    /// Total tokens consumed (prompt + completion).
    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }

    /// Estimated USD cost based on approximate per-token pricing for known models.
    ///
    /// Returns `None` for unrecognised models. Prices are rough approximations
    /// and may drift from the provider's actual billing.
    pub fn cost_estimate(&self, model: &str) -> Option<f64> {
        let m = model.to_lowercase();

        // (input_price_per_1m, output_price_per_1m)
        let (input_price, output_price) = if m.starts_with("gpt-4o-mini") {
            (0.15, 0.60)
        } else if m.starts_with("gpt-4o") {
            (2.50, 10.00)
        } else if m.starts_with("gpt-4-turbo") || m.starts_with("gpt-4-1") {
            (10.00, 30.00)
        } else if m.starts_with("gpt-4") {
            (30.00, 60.00)
        } else if m.starts_with("gpt-3.5") {
            (0.50, 1.50)
        } else if m.starts_with("o1-mini") {
            (3.00, 12.00)
        } else if m.starts_with("o1") || m.starts_with("o3") {
            (10.00, 40.00)
        } else if m.contains("claude-3-5-sonnet") || m.contains("claude-sonnet-4") {
            (3.00, 15.00)
        } else if m.contains("claude-3-5-haiku") || m.contains("claude-haiku-4") {
            (0.80, 4.00)
        } else if m.contains("claude-3-opus") || m.contains("claude-opus-4") {
            (15.00, 75.00)
        } else if m.starts_with("deepseek") {
            (0.27, 1.10)
        } else {
            return None;
        };

        let input_cost = f64::from(self.prompt_tokens) * input_price / 1_000_000.0;
        let output_cost = f64::from(self.completion_tokens) * output_price / 1_000_000.0;
        Some(input_cost + output_cost)
    }
}
