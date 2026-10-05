//! Layer-2 stream transforms: turn raw HTTP chunk streams into
//! [`SamplingEvent`](crate::events::SamplingEvent) streams.
//!
//! Each backend has its own transform because the raw chunk types
//! differ; backend dispatch happens in M4's
//! [`actor::request_task`](crate::actor::request_task), which knows
//! the API backend from `SamplerConfig.api_backend` and calls the
//! matching `SamplingClient::conversation_stream*` method before
//! handing the result to the corresponding transform here.

pub mod chat_completions;
pub mod collect;
pub mod messages;
pub mod responses;

pub use chat_completions::{stream_chat_completions, stream_chat_completions_for_provider};
pub use collect::collect_response;
pub use messages::stream_messages;
pub use responses::stream_responses;

/// Apply the selected provider's cost contract to every backend before delivery.
pub(crate) fn apply_provider_cost_policy(
    mut event: crate::events::SamplingEvent,
    profile: xai_grok_sampling_types::ProviderProfile,
) -> crate::events::SamplingEvent {
    if let crate::events::SamplingEvent::Completed { response, .. } = &mut event
        && !profile.xai_extensions()
    {
        response.cost_usd_ticks = None;
        if response.provider_cost.is_some_and(|c| c.source == xai_grok_sampling_types::CostSource::XaiUsageTicks) {
            response.provider_cost = None;
        }
    }
    event
}
