//! The operation layer: every thing a user can do, as a name plus a request type and a response
//! type. The daemon's wire format, the MCP tool schemas and the front ends' calls all come from
//! these definitions. Operations execute only in the daemon.

pub mod accounts;
pub mod actions;
pub mod body;
pub mod confirm;
pub mod daemon;
pub mod drafts;
pub mod error;
pub mod guide;
pub mod op;
pub mod pending;
pub mod presets;
pub mod secrets;
pub mod status;
pub mod sync;
pub mod untrusted;

#[cfg(test)]
mod drafts_tests;
#[cfg(test)]
mod tests;

pub use accounts::{
    AccountAddRequest, AccountRemoveRequest, AccountRemoveResponse, AgentAccountAdd,
    AgentAccountRemove, BeginResponse, ClientSecretInput, ConsentBeginRequest, LoginBeginRequest,
    NO_SECRETS_FROM_AGENTS, OP_REFERENCE_PREFIX, SavedAccount,
};
pub use actions::{
    Action, ActionApplyRequest, ActionApplyResponse, ActionFailed, ActionRetryRequest,
    ActionRetryResponse, ActionUndoRequest, ActionUndoResponse, DraftId, MAX_ACTION_LABELS,
    MAX_ACTION_THREADS, MAX_CLIENT_TOKEN_CHARS, MAX_UNDO_ROWS, OutboxDepth, OutboxId, RowToken,
    UNDO_GRACE, checked_token, client_token,
};
pub use body::{
    BodyFetchRequest, BodyFetchResponse, BodyFetchResult, BodyOutcome, MAX_BODY_FETCH,
    MAX_MESSAGE_ID_CHARS,
};
pub use confirm::{ConfirmPlan, MAX_NAME_CHARS, plan_digest, shown};
pub use daemon::{
    BrowserEnv, DaemonInfo, DaemonInfoRequest, DaemonStopRequest, DaemonStopResponse, HelloRequest,
    HelloResponse, MAX_BROWSER_ENV_CHARS,
};
pub use drafts::{
    AttachmentSummary, DraftAttachment, DraftDiscardRequest, DraftDiscardResponse,
    DraftImportRequest, DraftImportResponse, DraftPushRequest, DraftPushResponse, DraftSaveRequest,
    DraftSaveResponse, DraftSendRequest, DraftSendResponse, MAX_ADDRESS_CHARS, MAX_ATTACHMENTS,
    MAX_BODY_BYTES, MAX_MESSAGE_BYTES, MAX_RECIPIENTS, MAX_SUBJECT_BYTES, ReplyTo,
    estimated_raw_size, new_message_id, parse_recipient, too_large_text,
};
pub(crate) use error::write_config_text;
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
pub use sync::{
    AccountSyncStatus, AgentSyncNow, DEFAULT_SEARCH_LIMIT, HitSource, MAX_QUERY_CHARS,
    MAX_SEARCH_LIMIT, SearchFailure, SearchHit, SearchRequest, SearchResponse, SyncErrorInfo,
    SyncNowRequest, SyncNowResponse, SyncPhase, SyncStatusRequest, SyncStatusResponse, SyncTarget,
};
pub use untrusted::{Secret, Untrusted};

/// serde default for flags that are on unless the caller says otherwise.
pub(crate) fn yes() -> bool {
    true
}
