/// Usage tracking for Claude API rate limits and cost.
pub struct UsageTracker {
    /// Current rate limit utilization (0.0-1.0).
    pub utilization: f64,
    /// Rate limit status: "normal", "warning", "limited".
    pub status: String,
    /// Unix timestamp when rate limit resets.
    pub resets_at: Option<i64>,
    /// Total cost in USD across all turns.
    pub total_cost_usd: f64,
    /// Number of completed turns.
    pub turn_count: u32,
    /// Current context window usage in tokens.
    pub context_tokens: u64,
}

impl UsageTracker {
    pub fn new() -> Self {
        Self {
            utilization: 0.0,
            status: String::from("normal"),
            resets_at: None,
            total_cost_usd: 0.0,
            turn_count: 0,
            context_tokens: 0,
        }
    }

    pub fn record_rate_limit(&mut self, utilization: f64, status: String, resets_at: i64) {
        self.utilization = utilization;
        self.status = status;
        self.resets_at = Some(resets_at);
    }

    pub fn record_turn(&mut self, cost_usd: Option<f64>) {
        self.turn_count += 1;
        if let Some(cost) = cost_usd {
            self.total_cost_usd += cost;
        }
    }

    pub fn update_context(&mut self, tokens: u64) {
        self.context_tokens = tokens;
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}
