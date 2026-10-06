use crate::{CallRecord, CallStatus, CostSource, ReportedUsage};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use std::collections::BTreeMap;

/// Physical request attempts, including retries and auxiliaries. This NEVER
/// folds into the separate legacy accepted-main-response aggregate.
#[derive(Debug, Clone, PartialEq)]
pub struct CallLedger {
    pub history_complete: bool,
    pub recording_errors: u64,
    calls: Vec<CallRecord>,
}

impl CallLedger {
    pub fn new(history_complete: bool) -> Self {
        Self {
            history_complete,
            recording_errors: 0,
            calls: Vec::new(),
        }
    }

    pub fn calls(&self) -> &[CallRecord] {
        &self.calls
    }

    pub fn upsert(&mut self, call: CallRecord) {
        let Some(old) = self
            .calls
            .iter_mut()
            .find(|old| old.call_id == call.call_id)
        else {
            self.calls.push(call);
            return;
        };
        if !old.same_identity(&call)
            || (old.sequence == call.sequence && *old != call)
            || (old.sequence < call.sequence && old.status != CallStatus::Pending)
        {
            self.recording_errors = self.recording_errors.saturating_add(1);
        } else if old.sequence < call.sequence {
            *old = call;
        }
        // Older snapshots and identical duplicate child deliveries are no-ops.
    }

    pub fn merge(&mut self, other: &Self) {
        self.history_complete &= other.history_complete;
        // This is a completeness signal, not a repeatedly additive counter.
        self.recording_errors = self.recording_errors.max(other.recording_errors);
        for call in &other.calls {
            self.upsert(call.clone());
        }
    }

    pub fn summary(&self) -> CallSummary {
        let all = self.calls.iter().collect::<Vec<_>>();
        let (auxiliary, main): (Vec<_>, Vec<_>) = all
            .iter()
            .copied()
            .partition(|call| call.purpose.is_auxiliary());
        let complete_history = self.history_complete && self.recording_errors == 0;
        CallSummary {
            all: RequestTotals::from_calls(&all, complete_history),
            main: RequestTotals::from_calls(&main, complete_history),
            auxiliary: RequestTotals::from_calls(&auxiliary, complete_history),
        }
    }
}

// Summaries are always derived. Deserialization never trusts a saved subtotal
// over its records, and unknown schema versions cannot silently look complete.
impl Serialize for CallLedger {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("CallLedger", 5)?;
        out.serialize_field("schema_version", &1_u32)?;
        out.serialize_field("history_complete", &self.history_complete)?;
        out.serialize_field("recording_errors", &self.recording_errors)?;
        out.serialize_field("calls", &self.calls)?;
        out.serialize_field("summary", &self.summary())?;
        out.end()
    }
}

impl<'de> Deserialize<'de> for CallLedger {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Stored {
            schema_version: u32,
            #[serde(default)]
            history_complete: bool,
            #[serde(default)]
            recording_errors: u64,
            calls: Vec<CallRecord>,
        }
        let value = Stored::deserialize(deserializer)?;
        if value.schema_version != 1 {
            return Err(serde::de::Error::custom("unsupported request usage schema"));
        }
        let mut out = Self::new(value.history_complete);
        out.recording_errors = value.recording_errors;
        for call in value.calls {
            out.upsert(call);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallSummary {
    pub all: RequestTotals,
    pub main: RequestTotals,
    pub auxiliary: RequestTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CounterTotal {
    pub total: Option<u64>,
    pub known_total: Option<u64>,
    pub missing_calls: u64,
    pub overflow: bool,
}

impl CounterTotal {
    fn from_calls(
        calls: &[&CallRecord],
        complete: bool,
        select: impl Fn(&ReportedUsage) -> Option<u64>,
    ) -> Self {
        let mut sum = Some(0_u64);
        let mut known = 0_u64;
        let mut missing = 0_u64;
        for call in calls {
            if let Some(value) = call.usage.as_ref().and_then(&select) {
                known += 1;
                sum = sum.and_then(|sum| sum.checked_add(value));
            } else {
                missing += 1;
            }
        }
        let known_total = if known > 0 || calls.is_empty() {
            sum
        } else {
            None
        };
        Self {
            total: if complete && missing == 0 { sum } else { None },
            known_total,
            missing_calls: missing,
            overflow: sum.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestCost {
    pub total_usd: Option<f64>,
    pub known_usd: Option<f64>,
    pub cost_by_source: BTreeMap<CostSource, f64>,
    pub xai_cost_usd_ticks: Option<i64>,
    pub missing_calls: u64,
    pub invalid_or_overflow: bool,
}

impl RequestCost {
    fn from_calls(calls: &[&CallRecord], complete: bool) -> Self {
        let mut sources = BTreeMap::new();
        let mut ticks: Option<i64> = None;
        let mut missing = 0;
        let mut invalid = false;
        for call in calls {
            let Some(usage) = &call.usage else {
                missing += 1;
                continue;
            };
            let Some(cost) = usage.provider_cost else {
                missing += 1;
                continue;
            };
            let consistent = match cost.source {
                CostSource::XaiUsageTicks => {
                    call.provider == crate::ProviderProfile::Xai
                        && usage.cost_usd_ticks.is_some_and(|ticks| {
                            ticks > 0 && (cost.usd - ticks as f64 / 10_000_000_000.0).abs() <= 1e-10
                        })
                }
                CostSource::OpenrouterUsageCost => {
                    call.provider == crate::ProviderProfile::Openrouter
                        && usage.cost_usd_ticks.is_none()
                }
            };
            if !cost.usd.is_finite() || cost.usd < 0.0 || !consistent {
                invalid = true;
                missing += 1;
                continue;
            }
            *sources.entry(cost.source).or_insert(0.0) += cost.usd;
            if let Some(value) = usage.cost_usd_ticks {
                let next = ticks.unwrap_or(0).checked_add(value);
                invalid |= next.is_none();
                ticks = next;
            }
        }
        let sum: f64 = sources.values().sum();
        invalid |= !sum.is_finite();
        let known = (!invalid && (!sources.is_empty() || calls.is_empty())).then_some(sum);
        Self {
            total_usd: if complete && missing == 0 {
                known
            } else {
                None
            },
            known_usd: known,
            cost_by_source: sources,
            xai_cost_usd_ticks: if invalid { None } else { ticks },
            missing_calls: missing,
            invalid_or_overflow: invalid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestTotals {
    pub model_calls: u64,
    pub pending_calls: u64,
    pub failed_calls: u64,
    pub interrupted_calls: u64,
    pub usage_is_incomplete: bool,
    pub input_tokens: CounterTotal,
    pub uncached_input_tokens: CounterTotal,
    pub output_tokens: CounterTotal,
    pub cached_read_tokens: CounterTotal,
    pub cache_creation_tokens: CounterTotal,
    pub reasoning_tokens: CounterTotal,
    pub total_tokens: CounterTotal,
    pub cost: RequestCost,
}

impl RequestTotals {
    fn from_calls(calls: &[&CallRecord], complete_history: bool) -> Self {
        let count = |status| calls.iter().filter(|call| call.status == status).count() as u64;
        let complete = complete_history
            && calls.iter().all(|call| {
                call.status == CallStatus::Completed
                    && call.usage.as_ref().is_some_and(|u| {
                        !u.invalid_fields && u.input_tokens.is_some() && u.output_tokens.is_some()
                    })
            });
        Self {
            model_calls: calls.len() as u64,
            pending_calls: count(CallStatus::Pending),
            failed_calls: count(CallStatus::Failed),
            interrupted_calls: count(CallStatus::Interrupted),
            usage_is_incomplete: !complete,
            input_tokens: CounterTotal::from_calls(calls, complete, |u| u.input_tokens),
            uncached_input_tokens: CounterTotal::from_calls(calls, complete, |u| {
                u.uncached_input_tokens
            }),
            output_tokens: CounterTotal::from_calls(calls, complete, |u| u.output_tokens),
            cached_read_tokens: CounterTotal::from_calls(calls, complete, |u| u.cached_read_tokens),
            cache_creation_tokens: CounterTotal::from_calls(calls, complete, |u| {
                u.cache_creation_tokens
            }),
            reasoning_tokens: CounterTotal::from_calls(calls, complete, |u| u.reasoning_tokens),
            total_tokens: CounterTotal::from_calls(calls, complete, ReportedUsage::total_tokens),
            cost: RequestCost::from_calls(calls, complete),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallBackend, CallPurpose, ProviderProfile};
    use serde_json::json;

    fn call(
        id: &str,
        purpose: CallPurpose,
        provider: ProviderProfile,
        usage: serde_json::Value,
    ) -> CallRecord {
        let mut reported = ReportedUsage::default();
        reported.update(&usage, CallBackend::ChatCompletions, provider);
        CallRecord {
            call_id: id.into(),
            origin_session_id: "synthetic-session".into(),
            prompt_id: Some("synthetic-prompt".into()),
            model: "synthetic-model".into(),
            provider,
            backend: CallBackend::ChatCompletions,
            purpose,
            status: CallStatus::Completed,
            started_at_unix_ms: 1,
            api_duration_ms: Some(20),
            sequence: 2,
            usage: Some(reported),
        }
    }

    #[test]
    fn separate_auxiliary_cost_and_idempotent_child_fold() {
        let mut ledger = CallLedger::new(true);
        ledger.upsert(call(
            "main",
            CallPurpose::MainLoop,
            ProviderProfile::Openrouter,
            json!({"prompt_tokens":100,"completion_tokens":10,"cost":0.03}),
        ));
        let mut child = CallLedger::new(true);
        child.upsert(call(
            "aux",
            CallPurpose::Title,
            ProviderProfile::Xai,
            json!({"prompt_tokens":5,"completion_tokens":2,"cost_in_usd_ticks":20000000}),
        ));
        ledger.merge(&child);
        ledger.merge(&child);
        let summary = ledger.summary();
        assert_eq!(ledger.calls.len(), 2);
        assert_eq!(summary.main.cost.total_usd, Some(0.03));
        assert_eq!(summary.auxiliary.cost.total_usd, Some(0.002));
        assert_eq!(summary.all.cost.total_usd, Some(0.032));
        assert_eq!(summary.main.reasoning_tokens.total, None);
        assert_eq!(summary.all.cost.xai_cost_usd_ticks, Some(20000000));
    }

    #[test]
    fn pending_then_complete_survives_round_trip_without_trusting_totals() {
        let completed = call(
            "one",
            CallPurpose::MainLoop,
            ProviderProfile::Openrouter,
            json!({"prompt_tokens":10,"completion_tokens":2,"cost":0}),
        );
        let mut pending = completed.clone();
        pending.sequence = 0;
        pending.status = CallStatus::Pending;
        pending.usage = None;
        let mut ledger = CallLedger::new(true);
        ledger.upsert(pending.clone());
        assert_eq!(ledger.summary().all.cost.total_usd, None);
        assert_eq!(ledger.summary().all.input_tokens.known_total, None);
        ledger.upsert(completed);
        ledger.upsert(pending);
        let mut wire = serde_json::to_value(&ledger).unwrap();
        wire["summary"]["all"]["cost"]["total_usd"] = json!(999);
        let restored: CallLedger = serde_json::from_value(wire).unwrap();
        assert_eq!(restored.summary().all.cost.total_usd, Some(0.0));
        assert_eq!(restored.calls.len(), 1);
    }

    #[test]
    fn missing_usage_interruption_and_unknown_history_are_explicit() {
        let mut ledger = CallLedger::new(false);
        let mut row = call(
            "one",
            CallPurpose::MainLoop,
            ProviderProfile::Openrouter,
            json!({"prompt_tokens":10,"completion_tokens":2,"cost":0.01}),
        );
        row.status = CallStatus::Interrupted;
        ledger.upsert(row);
        let totals = ledger.summary().all;
        assert!(totals.usage_is_incomplete);
        assert_eq!(totals.cost.total_usd, None);
        assert_eq!(totals.cost.known_usd, Some(0.01));
        assert_eq!(totals.input_tokens.total, None);
        assert_eq!(totals.input_tokens.known_total, Some(10));
    }

    #[test]
    fn identity_conflict_and_overflow_cannot_create_a_complete_total() {
        let mut ledger = CallLedger::new(true);
        let row = call(
            "one",
            CallPurpose::MainLoop,
            ProviderProfile::Openrouter,
            json!({"prompt_tokens":u64::MAX,"completion_tokens":1,"cost":0.01}),
        );
        ledger.upsert(row.clone());
        let mut conflict = row.clone();
        conflict.model = "another-model".into();
        ledger.upsert(conflict);
        assert_eq!(ledger.recording_errors, 1);
        let mut second = row;
        second.call_id = "two".into();
        ledger.upsert(second);
        assert!(ledger.summary().all.input_tokens.overflow);
        assert_eq!(ledger.summary().all.input_tokens.total, None);
        assert_eq!(ledger.summary().all.input_tokens.known_total, None);
        assert_eq!(ledger.summary().all.cost.total_usd, None);
    }

    #[test]
    fn empty_usage_or_mismatched_cost_source_never_looks_complete() {
        let mut ledger = CallLedger::new(true);
        ledger.upsert(call(
            "empty",
            CallPurpose::MainLoop,
            ProviderProfile::Vllm,
            json!({}),
        ));
        assert!(ledger.summary().all.usage_is_incomplete);
        assert_eq!(ledger.summary().all.input_tokens.total, None);
        assert_eq!(ledger.summary().all.cost.known_usd, None);
        let mut fake = call(
            "fake",
            CallPurpose::Title,
            ProviderProfile::Openrouter,
            json!({"prompt_tokens":1,"completion_tokens":1,"cost":0}),
        );
        fake.provider = ProviderProfile::Vllm;
        ledger.upsert(fake);
        assert!(ledger.summary().all.cost.invalid_or_overflow);
        assert_eq!(ledger.summary().all.cost.total_usd, None);
    }
}
