//! Per-prompt and per-session billing ledgers.
//!
//! Session ledgers are serialized to `{session_dir}/usage.json` after each
//! billed mutation so reboot / TUI restart can restore them. Prompt ledgers
//! stay RAM-only (cleared on the next prompt).
//!
//! `total_tokens()` is input + output: Responses wire `total` is live context
//! length. Compaction and other side calls never call `record_main_loop_call`.
//!
//! # Completeness ownership
//!
//! Wire incomplete is the OR of these stores (each has a distinct role):
//!
//! - **`UsageLedger.incomplete`** — durable on the bill snapshot. Set by nested
//!   subagent incomplete fold, drain timeout, true apply-miss, and
//!   `mark_usage_incomplete`. Monotonic for a ledger instance.
//! - **Sticky (`subagent_usage_not_applied` on the coordinator)** — pin-scoped
//!   **report** signal (session-only attribution or apply-miss report). Not a
//!   second token sink; does not stain ledgers by itself.
//! - **Foreground live IDs** — fold may still land; freeze drains ≤120s or fails
//!   closed. Cancel skips multi-second drain (actor-loop safety).
//! - **Background live** — never waits; prompt report incomplete immediately;
//!   spend still folds into the session ledger at completion (no session-ledger
//!   incomplete).
//!
//! Freeze and cancel share one outcome policy: ledger marks only on fail-closed;
//! sticky and background_live are report-level only.
//!
//! Projection (`PromptUsage`) never invents tokens; it only ORs completeness
//! and scrubs costs when partial or incomplete.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use xai_grok_sampling_types::{CostSource, ProviderCost, TokenUsage};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub model_calls: u64,
    pub api_duration_ms: u64,
    /// USD ticks (1e10 per USD). Absent when no call reported cost.
    pub cost_usd_ticks: Option<i64>,
    /// Reported USD by source. Ticks above remain xAI-only, never invented from dollars.
    pub cost_by_source: BTreeMap<CostSource, f64>,
    pub cost_missing_calls: u64,
}

impl UsageTotals {
    fn from_call(
        usage: &TokenUsage,
        api_duration_ms: Option<u64>,
        cost_usd_ticks: Option<i64>,
        provider_cost: Option<ProviderCost>,
    ) -> Self {
        let cost_usd_ticks = xai_grok_sampling_types::reported_cost_ticks(cost_usd_ticks);
        let reported = provider_cost
            .filter(|c| c.usd.is_finite() && c.usd >= 0.0)
            .or_else(|| ProviderCost::from_xai_ticks(cost_usd_ticks));
        let cost_by_source = reported
            .map(|c| BTreeMap::from([(c.source, c.usd)]))
            .unwrap_or_default();
        Self {
            input_tokens: u64::from(usage.prompt_tokens),
            output_tokens: u64::from(usage.completion_tokens),
            cached_read_tokens: u64::from(usage.cached_prompt_tokens),
            cache_creation_tokens: u64::from(usage.cache_creation_prompt_tokens),
            reasoning_tokens: u64::from(usage.reasoning_tokens),
            model_calls: 1,
            api_duration_ms: api_duration_ms.unwrap_or(0),
            cost_usd_ticks,
            cost_by_source,
            cost_missing_calls: u64::from(reported.is_none()),
        }
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn cost_is_partial(&self) -> bool {
        self.known_cost_usd().is_some() && self.cost_missing_calls > 0
    }

    pub fn reported_costs(&self) -> BTreeMap<CostSource, f64> {
        if self.cost_by_source.is_empty() {
            ProviderCost::from_xai_ticks(self.cost_usd_ticks)
                .map(|c| BTreeMap::from([(c.source, c.usd)]))
                .unwrap_or_default()
        } else {
            self.cost_by_source.clone()
        }
    }

    pub fn known_cost_usd(&self) -> Option<f64> {
        let costs = self.reported_costs();
        if costs.is_empty() || costs.values().any(|v| !v.is_finite() || *v < 0.0) {
            return None;
        }
        match (costs.get(&CostSource::XaiUsageTicks), self.cost_usd_ticks) {
            (Some(amount), Some(ticks))
                if ticks > 0 && (*amount - ticks as f64 / 10_000_000_000.0).abs() <= 1e-10 => {}
            (None, None) => {}
            _ => return None,
        }
        let value: f64 = costs.values().sum();
        value.is_finite().then_some(value)
    }

    fn fold_totals(&mut self, other: &UsageTotals) {
        let invalid_cost = [&*self, other].iter().any(|row| {
            (row.cost_usd_ticks.is_some() || !row.cost_by_source.is_empty())
                && row.known_cost_usd().is_none()
        });
        let tick_overflow = self
            .cost_usd_ticks
            .zip(other.cost_usd_ticks)
            .is_some_and(|(a, b)| a.checked_add(b).is_none());
        let Self {
            input_tokens,
            output_tokens,
            cached_read_tokens,
            cache_creation_tokens,
            reasoning_tokens,
            model_calls,
            api_duration_ms,
            cost_usd_ticks,
            cost_by_source: _,
            cost_missing_calls,
        } = other;
        let mut costs = self.reported_costs();
        for (source, amount) in other.reported_costs() {
            *costs.entry(source).or_default() += amount;
        }
        self.cost_by_source = costs;
        self.input_tokens = self.input_tokens.saturating_add(*input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(*output_tokens);
        self.cached_read_tokens = self.cached_read_tokens.saturating_add(*cached_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(*cache_creation_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(*reasoning_tokens);
        self.model_calls = self.model_calls.saturating_add(*model_calls);
        self.api_duration_ms = self.api_duration_ms.saturating_add(*api_duration_ms);
        self.cost_missing_calls = self.cost_missing_calls.saturating_add(*cost_missing_calls);
        self.cost_usd_ticks = merge_cost_ticks(self.cost_usd_ticks, *cost_usd_ticks);
        if let Some(cost) = ProviderCost::from_xai_ticks(self.cost_usd_ticks) {
            self.cost_by_source.insert(cost.source, cost.usd);
        }
        if invalid_cost
            || tick_overflow
            || (!self.cost_by_source.is_empty() && self.known_cost_usd().is_none())
        {
            // Overflow/invalid loaded metadata cannot turn into a valid free total.
            self.cost_by_source.clear();
            self.cost_usd_ticks = None;
            self.cost_missing_calls = self.model_calls;
        }
    }
}

fn merge_cost_ticks(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => a.unwrap_or(0).checked_add(b.unwrap_or(0)),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageLedger {
    pub totals: UsageTotals,
    pub by_model: IndexMap<String, UsageTotals>,
    /// Main-agent loop rounds for `num_turns` (subagents excluded).
    pub main_loop_model_calls: u64,
    /// Bill may under-count (drain timeout, nested subagent incomplete, apply failure).
    pub incomplete: bool,
}

impl UsageLedger {
    /// Fold one main-agent-loop model call. This is the only writer of
    /// `main_loop_model_calls` (the wire `numTurns`); side calls such as
    /// compaction must not use it.
    pub fn record_main_loop_call(
        &mut self,
        model_id: &str,
        usage: &TokenUsage,
        api_duration_ms: Option<u64>,
        cost_usd_ticks: Option<i64>,
    ) {
        self.record_provider_call(
            model_id,
            usage,
            api_duration_ms,
            cost_usd_ticks,
            ProviderCost::from_xai_ticks(cost_usd_ticks),
        );
    }

    pub fn record_provider_call(
        &mut self,
        model_id: &str,
        usage: &TokenUsage,
        api_duration_ms: Option<u64>,
        cost_usd_ticks: Option<i64>,
        provider_cost: Option<ProviderCost>,
    ) {
        let call = UsageTotals::from_call(usage, api_duration_ms, cost_usd_ticks, provider_cost);
        self.main_loop_model_calls = self.main_loop_model_calls.saturating_add(1);
        self.fold_entry(model_id, &call);
    }

    /// Fold subagent usage without incrementing `main_loop_model_calls`.
    pub fn record_subagent(&mut self, by_model: &[(String, UsageTotals)], incomplete: bool) {
        for (model_id, totals) in by_model {
            self.fold_entry(model_id, totals);
        }
        if incomplete {
            self.incomplete = true;
        }
    }

    pub fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }

    fn fold_entry(&mut self, model_id: &str, totals: &UsageTotals) {
        self.totals.fold_totals(totals);
        self.by_model
            .entry(model_id.to_owned())
            .or_default()
            .fold_totals(totals);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tu(prompt: u32, completion: u32) -> TokenUsage {
        TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: 999_999,
            reasoning_tokens: 0,
            cached_prompt_tokens: 0,
            cache_creation_prompt_tokens: 0,
        }
    }

    #[test]
    fn a4_provider_ledger_preserves_zero_unknown_mixed_sources_and_legacy() {
        let mut ledger = UsageLedger::default();
        let usage = TokenUsage {
            reasoning_tokens: 3,
            cached_prompt_tokens: 2,
            cache_creation_prompt_tokens: 1,
            ..tu(10, 5)
        };
        ledger.record_provider_call(
            "router",
            &usage,
            Some(7),
            None,
            Some(ProviderCost {
                usd: 0.0,
                source: CostSource::OpenrouterUsageCost,
            }),
        );
        assert_eq!(ledger.totals.known_cost_usd(), Some(0.0));
        assert_eq!(ledger.totals.cost_missing_calls, 0);
        assert!(!ledger.totals.cost_is_partial());
        ledger.record_provider_call(
            "router",
            &usage,
            Some(7),
            None,
            Some(ProviderCost {
                usd: 0.2,
                source: CostSource::OpenrouterUsageCost,
            }),
        );
        ledger.record_main_loop_call("xai", &usage, Some(7), Some(1_000_000_000));
        assert!((ledger.totals.known_cost_usd().unwrap() - 0.3).abs() < 1e-12);
        assert_eq!(ledger.totals.cost_usd_ticks, Some(1_000_000_000));
        assert_eq!(ledger.totals.reasoning_tokens, 9);
        assert_eq!(ledger.totals.cache_creation_tokens, 3);
        let restored: UsageLedger =
            serde_json::from_str(&serde_json::to_string(&ledger).unwrap()).unwrap();
        assert_eq!(restored, ledger);
        ledger.record_provider_call("local", &usage, None, None, None);
        assert!(ledger.totals.cost_is_partial());
        assert_eq!(ledger.totals.cost_missing_calls, 1);
        let legacy: UsageTotals = serde_json::from_value(serde_json::json!({
            "model_calls": 1, "cost_usd_ticks": 1_000_000_000
        }))
        .unwrap();
        assert_eq!(legacy.known_cost_usd(), Some(0.1));
        assert_eq!(legacy.reported_costs().len(), 1);
    }

    #[test]
    fn a4_invalid_or_overflowed_ledger_money_fails_closed() {
        let mut ledger = UsageLedger::default();
        ledger.record_main_loop_call("xai", &tu(1, 1), None, Some(i64::MAX));
        ledger.record_main_loop_call("xai", &tu(1, 1), None, Some(1));
        assert_eq!(ledger.totals.known_cost_usd(), None);
        assert_eq!(ledger.totals.cost_usd_ticks, None);
        assert_eq!(ledger.totals.cost_missing_calls, 2);
        let invalid = UsageTotals {
            cost_usd_ticks: Some(10),
            cost_by_source: BTreeMap::from([(CostSource::XaiUsageTicks, 42.0)]),
            model_calls: 1,
            ..Default::default()
        };
        assert_eq!(invalid.known_cost_usd(), None);
        ledger.record_subagent(&[("invalid".into(), invalid)], false);
        assert_eq!(ledger.totals.known_cost_usd(), None);
        assert_eq!(ledger.totals.cost_missing_calls, 3);
    }

    #[test]
    fn ledger_sums_partial_subagent_and_zero_cost() {
        let mut ledger = UsageLedger::default();
        ledger.record_main_loop_call("m", &tu(1, 1), None, Some(0));
        assert_eq!(ledger.totals.cost_usd_ticks, None);
        assert_eq!(ledger.totals.cost_missing_calls, 1);

        ledger.record_main_loop_call("a", &tu(100, 10), Some(100), None);
        ledger.record_main_loop_call("a", &tu(50, 5), Some(50), Some(70));
        assert_eq!(ledger.totals.cost_usd_ticks, Some(70));
        assert!(ledger.totals.cost_is_partial());
        assert_eq!(ledger.main_loop_model_calls, 3);

        ledger.record_subagent(
            &[(
                "b".into(),
                UsageTotals {
                    input_tokens: 5,
                    model_calls: 1,
                    ..Default::default()
                },
            )],
            false,
        );
        assert_eq!(ledger.by_model["b"].input_tokens, 5);
        assert_eq!(ledger.main_loop_model_calls, 3);
        assert_eq!(ledger.totals.model_calls, 4);
        assert!(!ledger.incomplete);

        ledger.record_subagent(&[], true);
        assert!(ledger.incomplete);
    }

    #[test]
    fn usage_json_roundtrip_keeps_stable_field_names() {
        let mut ledger = UsageLedger::default();
        ledger.record_main_loop_call("m", &tu(10, 4), Some(12), None);
        ledger.record_main_loop_call(
            "m",
            &TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 1,
                total_tokens: 0,
                reasoning_tokens: 2,
                cached_prompt_tokens: 1,
                cache_creation_prompt_tokens: 0,
            },
            Some(8),
            Some(50),
        );
        ledger.record_subagent(
            &[(
                "child".into(),
                UsageTotals {
                    input_tokens: 7,
                    output_tokens: 2,
                    cached_read_tokens: 1,
                    model_calls: 1,
                    ..Default::default()
                },
            )],
            true,
        );

        let json = serde_json::to_value(&ledger).expect("serialize");
        assert!(json.get("totals").is_some());
        assert!(json.get("by_model").is_some());
        assert_eq!(json["main_loop_model_calls"], 2);
        assert_eq!(json["incomplete"], true);
        assert_eq!(json["totals"]["input_tokens"], 20);
        assert_eq!(json["totals"]["output_tokens"], 7);
        assert_eq!(json["totals"]["cached_read_tokens"], 2);
        assert_eq!(json["totals"]["model_calls"], 3);

        let restored: UsageLedger = serde_json::from_value(json).expect("deserialize");
        assert_eq!(restored, ledger);
    }

    #[test]
    fn missing_usage_json_fields_deserialize_as_empty_ledger() {
        let restored: UsageLedger = serde_json::from_str("{}").expect("empty object");
        assert_eq!(restored, UsageLedger::default());
    }
}
