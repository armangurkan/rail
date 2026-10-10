//! `presets.list` and `preset.fetch`. A preset's client secret never leaves the daemon: the
//! summary says only whether there is one.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::OpError;
use super::untrusted::Untrusted;
use crate::config::AccountKind;
use crate::onboarding::Preset;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetsListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetsListResponse {
    /// Every `*.toml` file found, in discovery order, including the ones that failed to parse.
    pub presets: Vec<PresetListing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetListing {
    /// The file path.
    pub source: String,
    pub preset: Option<PresetSummary>,
    /// Why the file could not be used: its path, line and a fixed reason.
    pub error: Option<OpError>,
}

/// A preset without its client secret. Every field read from the preset file is outside text,
/// so it is `Untrusted`; only the kind (parsed into an enum) and the flag are Rail's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetSummary {
    pub name: Option<Untrusted<String>>,
    /// The name, or "kind (client id)" without one.
    pub label: Untrusted<String>,
    pub kind: AccountKind,
    pub client_id: Untrusted<String>,
    pub tenant: Option<Untrusted<String>>,
    /// True when the preset carries a client secret. Pass the preset's source as the client
    /// secret input to use it.
    pub has_client_secret: bool,
    pub help_url: Option<Untrusted<String>>,
}

impl PresetSummary {
    pub fn from_preset(preset: &Preset) -> Self {
        Self {
            name: preset.name.clone().map(Untrusted::new),
            label: Untrusted::new(preset.label()),
            kind: preset.kind,
            client_id: Untrusted::new(preset.client_id.clone()),
            tenant: preset.tenant.clone().map(Untrusted::new),
            has_client_secret: preset.client_secret.is_some(),
            help_url: preset.help_url.clone().map(Untrusted::new),
        }
    }
}

/// `preset.fetch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetFetchRequest {
    /// An `https://` URL (plain `http` is refused) or a file path.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetFetchResponse {
    pub source: String,
    pub preset: PresetSummary,
}
