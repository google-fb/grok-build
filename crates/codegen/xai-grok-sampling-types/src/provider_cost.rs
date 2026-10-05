//! Provider-reported USD amounts. No token pricing or inferred billing rates.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    XaiUsageTicks,
    OpenrouterUsageCost,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProviderCost {
    /// OpenRouter credits are USD-denominated; this excludes top-up fees and
    /// separately billed BYOK upstream charges. Never substitute upstream cost.
    pub usd: f64,
    pub source: CostSource,
}

impl ProviderCost {
    pub fn from_xai_ticks(raw: Option<i64>) -> Option<Self> {
        crate::reported_cost_ticks(raw).map(|ticks| Self {
            usd: ticks as f64 / 10_000_000_000.0,
            source: CostSource::XaiUsageTicks,
        })
    }

    /// Missing and explicit null mean unknown. A numeric zero is reported free
    /// usage. Invalid metadata is an error, never silently converted to zero.
    pub fn from_openrouter(
        value: Option<&serde_json::Value>,
    ) -> Result<Option<Self>, &'static str> {
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let usd = value
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or("invalid provider-reported USD amount")?;
        Ok(Some(Self {
            usd,
            source: CostSource::OpenrouterUsageCost,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a4_provider_money_distinguishes_absence_zero_and_invalid() {
        assert_eq!(ProviderCost::from_openrouter(None).unwrap(), None);
        assert_eq!(
            ProviderCost::from_openrouter(Some(&json!(null))).unwrap(),
            None
        );
        assert_eq!(
            ProviderCost::from_openrouter(Some(&json!(0)))
                .unwrap()
                .unwrap()
                .usd,
            0.0
        );
        assert_eq!(
            ProviderCost::from_openrouter(Some(&json!(0.0123)))
                .unwrap()
                .unwrap()
                .usd,
            0.0123
        );
        for value in [json!(-1), json!(true), json!("0.1"), json!({}), json!([])] {
            assert!(ProviderCost::from_openrouter(Some(&value)).is_err());
        }
        assert_eq!(ProviderCost::from_xai_ticks(Some(0)), None);
        assert_eq!(ProviderCost::from_xai_ticks(Some(-1)), None);
        assert_eq!(ProviderCost::from_xai_ticks(Some(1)).unwrap().usd, 1e-10);
    }
}
