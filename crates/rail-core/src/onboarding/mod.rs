//! Onboarding: preset files an organisation publishes, and the wizard's pure state machine.

pub mod draft;
pub mod events;
pub mod instructions;
pub mod preset;
pub mod view;
pub mod wizard;
#[cfg(test)]
mod wizard_tests;

pub use draft::{AccountDraft, Field, SHARED_TENANTS, is_address, is_concrete_tenant};
pub use events::{Action, DoneReport, Failure, Input, PresetEntry, Remedy};
pub use instructions::{REDIRECT_URI, instructions, portal_url};
pub use preset::{
    MAX_PRESET_BYTES, Preset, PresetError, SYSTEM_PRESET_DIR, discover_presets,
    discover_presets_in, fetch_preset, parse_preset, preset_dirs, read_preset,
};
pub use view::{Body, FailureView, FieldView, Primary, STEP_LABELS, StepState, ViewModel};
pub use wizard::{Busy, DONE_CHOICES, PROVIDER_CHOICES, START_CHOICES, Step, Wizard};
