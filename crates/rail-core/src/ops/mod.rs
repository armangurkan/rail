//! The operation layer: every thing a user can do, as a name plus a request type and a response
//! type. The daemon's wire format, the MCP tool schemas and the front ends' calls all come from
//! these definitions. Operations execute only in the daemon.

pub mod accounts;
pub mod daemon;
pub mod error;
pub mod guide;
pub mod op;
pub mod pending;
pub mod presets;
pub mod secrets;
pub mod status;
pub mod untrusted;

#[cfg(test)]
mod tests;

pub use accounts::{
    AccountAddRequest, AccountRemoveRequest, AccountRemoveResponse, AgentAccountAdd,
    AgentAccountRemove, BeginResponse, ClientSecretInput, ConsentBeginRequest, LoginBeginRequest,
    OP_REFERENCE_PREFIX, SavedAccount,
};
pub use daemon::{
    DaemonInfo, DaemonInfoRequest, DaemonStopRequest, DaemonStopResponse, HelloRequest,
    HelloResponse,
};
pub use error::{ErrorCode, OpError, preset_file_message};
pub use guide::{Audience, GuideInputs, GuideRequest, GuideResponse, GuideStep, StepKind};
pub use op::{Operation, names};
pub use pending::{
    CancelRequest, MAX_WAIT_SECS, PendingId, PendingKind, PendingOutcome, PendingState,
    WaitRequest, WaitResponse,
};
pub use presets::{
    PresetFetchRequest, PresetFetchResponse, PresetListing, PresetSummary, PresetsListRequest,
    PresetsListResponse,
};
pub use secrets::{SecretsConfigureRequest, SecretsConfigureResponse};
pub use status::{
    AccountStatus, AccountSummary, AccountsListRequest, AccountsListResponse, DoctorReport,
    StatusRequest, StatusResponse, TokenState,
};
pub use untrusted::{Secret, Untrusted};

/// serde default for flags that are on unless the caller says otherwise.
pub(crate) fn yes() -> bool {
    true
}
