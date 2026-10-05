//! Explicit provider presets for protocol extensions, never inferred from a model name.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProfile {
    #[default]
    Compatible,
    Xai,
    Openrouter,
    Vllm,
}

impl ProviderProfile {
    pub fn xai_extensions(self) -> bool {
        self == Self::Xai
    }
}
