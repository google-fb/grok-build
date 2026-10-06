//! Provider-reported accounting only: never request text, response text, URLs,
//! credentials, or estimated prices. Physical attempts are a separate ledger
//! from the application's accepted-main-response totals.

mod ledger;
mod observer;
mod provider;
mod provider_cost;
mod record;
pub use ledger::*;
pub use observer::*;
pub use provider::*;
pub use provider_cost::*;
pub use record::*;

pub fn reported_cost_ticks(raw: Option<i64>) -> Option<i64> {
    raw.filter(|&ticks| ticks > 0)
}

pub mod http;
