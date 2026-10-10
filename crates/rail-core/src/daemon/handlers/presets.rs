//! `presets.list` and `preset.fetch`. A preset's client secret stays in the daemon, and a file
//! that does not parse is reported by path, line and a fixed reason, never by its contents.

use std::path::PathBuf;

use crate::accounts::{is_url, require_absolute};
use crate::daemon::registry::Call;
use crate::onboarding::{PresetError, discover_presets, fetch_preset, read_preset};
use crate::ops::{
    OpError, PresetFetchRequest, PresetFetchResponse, PresetListing, PresetSummary,
    PresetsListRequest, PresetsListResponse, Untrusted,
};

pub(super) async fn presets_list(
    call: Call,
    _: PresetsListRequest,
) -> Result<PresetsListResponse, OpError> {
    let paths = call.ctx.paths.clone();
    let found = call
        .ctx
        .blocking(move || Ok(discover_presets(&paths)))
        .await?;
    let presets = found
        .into_iter()
        .map(|(source, preset)| PresetListing {
            source: Untrusted::new(source.display().to_string()),
            preset: preset.as_ref().ok().map(PresetSummary::from_preset),
            error: preset.err().map(|e| PresetError::Parse(e).into()),
        })
        .collect();
    Ok(PresetsListResponse { presets })
}

/// A URL (it must be https) is fetched; anything else is read as a file path.
pub(super) async fn preset_fetch(
    call: Call,
    request: PresetFetchRequest,
) -> Result<PresetFetchResponse, OpError> {
    let source = request.source.trim().to_string();
    let preset = if is_url(&source) {
        fetch_preset(&call.ctx.http, &source).await?
    } else {
        let path = PathBuf::from(&source);
        require_absolute(&path)?;
        call.ctx
            .blocking(move || read_preset(&path).map_err(|e| PresetError::Parse(e).into()))
            .await?
    };
    Ok(PresetFetchResponse {
        source,
        preset: PresetSummary::from_preset(&preset),
    })
}
