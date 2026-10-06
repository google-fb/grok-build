use crate::{ProviderCost, ProviderProfile, reported_cost_ticks};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallBackend {
    ChatCompletions,
    Responses,
    Messages,
    Embeddings,
    Images,
    Videos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPurpose {
    MainLoop,
    Auxiliary,
    Compaction,
    SessionSummary,
    Title,
    TurnSummary,
    PermissionClassifier,
    Memory,
    ImageDescription,
    SideQuestion,
    PromptSuggestion,
    Goal,
    WebSearch,
    Embedding,
    ImageGeneration,
    ImageEdit,
    VideoGeneration,
}

impl CallPurpose {
    pub fn is_auxiliary(self) -> bool {
        self != Self::MainLoop
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Pending,
    Completed,
    Failed,
    Interrupted,
}

/// Every absent provider counter stays null. `input_tokens` includes cache;
/// `uncached_input_tokens` excludes it. Either can be unavailable when the
/// provider omits the counters needed to convert its input convention.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReportedUsage {
    pub input_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    /// Original wire total. Never substitute this for input + output: some
    /// services use a context-window total instead of cumulative billing.
    pub provider_total_tokens: Option<u64>,
    pub cost_usd_ticks: Option<i64>,
    pub provider_cost: Option<ProviderCost>,
    pub invalid_fields: bool,
}

fn counter(value: Option<&Value>, invalid: &mut bool) -> Option<u64> {
    match value.filter(|v| !v.is_null()) {
        None => None,
        Some(value) => {
            let result = value.as_u64();
            *invalid |= result.is_none();
            result
        }
    }
}

impl ReportedUsage {
    /// Merge cumulative usage snapshots (including partial Messages deltas).
    /// A repeated snapshot replaces counters rather than adding them. Parsing
    /// accepts ONLY the usage object, so bodies cannot enter the record.
    pub fn update(&mut self, value: &Value, backend: CallBackend, provider: ProviderProfile) {
        if !value.is_object() {
            self.invalid_fields = true;
            return;
        }
        let (input, output, cached, creation, reasoning) = match backend {
            CallBackend::ChatCompletions => (
                "/prompt_tokens",
                "/completion_tokens",
                "/prompt_tokens_details/cached_tokens",
                "/prompt_tokens_details/cache_write_tokens",
                "/completion_tokens_details/reasoning_tokens",
            ),
            CallBackend::Responses => (
                "/input_tokens",
                "/output_tokens",
                "/input_tokens_details/cached_tokens",
                "/input_tokens_details/cache_write_tokens",
                "/output_tokens_details/reasoning_tokens",
            ),
            CallBackend::Messages => (
                "/input_tokens",
                "/output_tokens",
                "/cache_read_input_tokens",
                "/cache_creation_input_tokens",
                "/reasoning_tokens",
            ),
            CallBackend::Embeddings => (
                "/prompt_tokens",
                "/completion_tokens",
                "/prompt_tokens_details/cached_tokens",
                "/prompt_tokens_details/cache_write_tokens",
                "/completion_tokens_details/reasoning_tokens",
            ),
            CallBackend::Images | CallBackend::Videos => (
                "/input_tokens",
                "/output_tokens",
                "/input_tokens_details/cached_tokens",
                "/input_tokens_details/cache_write_tokens",
                "/output_tokens_details/reasoning_tokens",
            ),
        };
        let mut read = |path| counter(value.pointer(path), &mut self.invalid_fields);
        let new_input = read(input);
        for (slot, path) in [
            (&mut self.output_tokens, output),
            (&mut self.cached_read_tokens, cached),
            (&mut self.cache_creation_tokens, creation),
            (&mut self.reasoning_tokens, reasoning),
            (&mut self.provider_total_tokens, "/total_tokens"),
        ] {
            if value.pointer(path).is_some() {
                *slot = read(path);
            }
        }
        if backend == CallBackend::Messages {
            if value.pointer(input).is_some() {
                self.uncached_input_tokens = new_input;
            }
            self.input_tokens = self
                .uncached_input_tokens
                .zip(self.cached_read_tokens)
                .zip(self.cache_creation_tokens)
                .and_then(|((fresh, read), write)| fresh.checked_add(read)?.checked_add(write));
        } else {
            if value.pointer(input).is_some() {
                self.input_tokens = new_input;
            }
            self.uncached_input_tokens = self
                .input_tokens
                .zip(self.cached_read_tokens)
                .zip(self.cache_creation_tokens)
                .and_then(|((input, read), write)| input.checked_sub(read)?.checked_sub(write));
        }
        self.invalid_fields |= self
            .input_tokens
            .zip(self.output_tokens)
            .is_some_and(|(input, output)| input.checked_add(output).is_none());
        if backend == CallBackend::Messages {
            self.invalid_fields |= self.uncached_input_tokens.is_some()
                && self.cached_read_tokens.is_some()
                && self.cache_creation_tokens.is_some()
                && self.input_tokens.is_none();
        } else if let Some(input) = self.input_tokens {
            self.invalid_fields |= self.cached_read_tokens.is_some_and(|n| n > input)
                || self.cache_creation_tokens.is_some_and(|n| n > input)
                || (self.cached_read_tokens.is_some()
                    && self.cache_creation_tokens.is_some()
                    && self.uncached_input_tokens.is_none());
        }
        match provider {
            ProviderProfile::Xai => {
                if let Some(value) = value.get("cost_in_usd_ticks").filter(|v| !v.is_null()) {
                    let raw = value.as_i64();
                    self.invalid_fields |= raw.is_none() || raw.is_some_and(|v| v < 0);
                    self.cost_usd_ticks = reported_cost_ticks(raw);
                    self.provider_cost = ProviderCost::from_xai_ticks(self.cost_usd_ticks);
                }
            }
            ProviderProfile::Openrouter => {
                if let Some(value) = value.get("cost") {
                    match ProviderCost::from_openrouter(Some(value)) {
                        Ok(cost) => self.provider_cost = cost,
                        Err(_) => {
                            self.provider_cost = None;
                            self.invalid_fields = true;
                        }
                    }
                }
            }
            ProviderProfile::Compatible | ProviderProfile::Vllm => {}
        }
    }

    pub fn total_tokens(&self) -> Option<u64> {
        self.input_tokens?.checked_add(self.output_tokens?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallRecord {
    pub call_id: String,
    pub origin_session_id: String,
    pub prompt_id: Option<String>,
    pub model: String,
    pub provider: ProviderProfile,
    pub backend: CallBackend,
    pub purpose: CallPurpose,
    pub status: CallStatus,
    pub started_at_unix_ms: u64,
    pub api_duration_ms: Option<u64>,
    pub sequence: u64,
    pub usage: Option<ReportedUsage>,
}

impl CallRecord {
    pub fn same_identity(&self, other: &Self) -> bool {
        self.call_id == other.call_id
            && self.origin_session_id == other.origin_session_id
            && self.prompt_id == other.prompt_id
            && self.model == other.model
            && self.provider == other.provider
            && self.backend == other.backend
            && self.purpose == other.purpose
            && self.started_at_unix_ms == other.started_at_unix_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_missing_zero_details_and_cumulative_values() {
        let mut usage = ReportedUsage::default();
        usage.update(
            &json!({"prompt_tokens": 100, "completion_tokens": 10, "cost": 0}),
            CallBackend::ChatCompletions,
            ProviderProfile::Openrouter,
        );
        assert_eq!(usage.cached_read_tokens, None);
        assert_eq!(usage.reasoning_tokens, None);
        assert_eq!(usage.uncached_input_tokens, None);
        assert_eq!(usage.provider_cost.unwrap().usd, 0.0);
        let value = json!({"prompt_tokens":100,"completion_tokens":10,
            "prompt_tokens_details":{"cached_tokens":20,"cache_write_tokens":5},
            "completion_tokens_details":{"reasoning_tokens":0},"total_tokens":999});
        usage.update(
            &value,
            CallBackend::ChatCompletions,
            ProviderProfile::Openrouter,
        );
        usage.update(
            &value,
            CallBackend::ChatCompletions,
            ProviderProfile::Openrouter,
        );
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.uncached_input_tokens, Some(75));
        assert_eq!(usage.reasoning_tokens, Some(0));
        assert_eq!(usage.total_tokens(), Some(110));
        assert_eq!(usage.provider_total_tokens, Some(999));
    }

    #[test]
    fn messages_partial_deltas_keep_input_and_cache_separate() {
        let mut usage = ReportedUsage::default();
        usage.update(
            &json!({"input_tokens":20,"output_tokens":0}),
            CallBackend::Messages,
            ProviderProfile::Compatible,
        );
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.uncached_input_tokens, Some(20));
        usage.update(
            &json!({"cache_read_input_tokens":10,"cache_creation_input_tokens":0,
            "output_tokens":5}),
            CallBackend::Messages,
            ProviderProfile::Compatible,
        );
        assert_eq!(usage.input_tokens, Some(30));
        assert_eq!(usage.total_tokens(), Some(35));
    }

    #[test]
    fn invalid_counter_and_money_never_become_free_usage() {
        for value in [json!(-1), json!("0"), json!(true)] {
            let mut usage = ReportedUsage::default();
            usage.update(
                &json!({"prompt_tokens":value,"cost":value}),
                CallBackend::ChatCompletions,
                ProviderProfile::Openrouter,
            );
            assert!(usage.invalid_fields);
            assert_eq!(usage.input_tokens, None);
            assert_eq!(usage.provider_cost, None);
        }
        let mut usage = ReportedUsage::default();
        usage.update(
            &json!({"prompt_tokens":1,"cost":9,"cost_in_usd_ticks":90}),
            CallBackend::ChatCompletions,
            ProviderProfile::Vllm,
        );
        assert_eq!(usage.provider_cost, None);
        assert_eq!(usage.cost_usd_ticks, None);
    }

    #[test]
    fn explicit_null_withdraws_a_counter_and_inconsistent_cache_is_invalid() {
        let mut usage = ReportedUsage::default();
        usage.update(
            &json!({"prompt_tokens":10,"completion_tokens":2}),
            CallBackend::ChatCompletions,
            ProviderProfile::Compatible,
        );
        usage.update(
            &json!({"completion_tokens":null}),
            CallBackend::ChatCompletions,
            ProviderProfile::Compatible,
        );
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, None);
        usage.update(
            &json!({"prompt_tokens_details":{"cached_tokens":11}}),
            CallBackend::ChatCompletions,
            ProviderProfile::Compatible,
        );
        assert!(usage.invalid_fields);
        assert_eq!(usage.uncached_input_tokens, None);
    }
}
