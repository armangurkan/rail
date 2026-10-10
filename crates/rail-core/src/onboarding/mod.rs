//! Onboarding: preset files an organisation publishes, and the wizard's pure state machine.

pub mod preset;

pub use preset::{
    MAX_PRESET_BYTES, Preset, PresetError, SYSTEM_PRESET_DIR, discover_presets,
    discover_presets_in, fetch_preset, parse_preset, preset_dirs, read_preset,
};
