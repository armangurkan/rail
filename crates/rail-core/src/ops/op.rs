//! Operation names and the `Operation` trait that ties a name to its request and response.

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::accounts::{
    AccountAddRequest, AccountRemoveRequest, AccountRemoveResponse, BeginResponse,
    ConsentBeginRequest, LoginBeginRequest,
};
use super::daemon::{
    DaemonInfo, DaemonInfoRequest, DaemonStopRequest, DaemonStopResponse, HelloRequest,
    HelloResponse,
};
use super::guide::{GuideRequest, GuideResponse};
use super::pending::{CancelRequest, WaitRequest, WaitResponse};
use super::presets::{
    PresetFetchRequest, PresetFetchResponse, PresetsListRequest, PresetsListResponse,
};
use super::secrets::{SecretsConfigureRequest, SecretsConfigureResponse};
use super::status::{AccountsListRequest, AccountsListResponse, StatusRequest, StatusResponse};
use super::sync::{
    SearchRequest, SearchResponse, SyncNowRequest, SyncNowResponse, SyncStatusRequest,
    SyncStatusResponse,
};

/// The wire names. They never change once released.
pub mod names {
    pub const STATUS: &str = "status";
    pub const ACCOUNTS_LIST: &str = "accounts.list";
    pub const PRESETS_LIST: &str = "presets.list";
    pub const PRESET_FETCH: &str = "preset.fetch";
    pub const ACCOUNT_ADD_BEGIN: &str = "account.add.begin";
    pub const ACCOUNT_LOGIN_BEGIN: &str = "account.login.begin";
    pub const CONSENT_BEGIN: &str = "consent.begin";
    pub const ACCOUNT_REMOVE: &str = "account.remove";
    pub const SECRETS_CONFIGURE: &str = "secrets.configure";
    pub const GUIDE_PROVIDER: &str = "guide.provider";
    pub const PENDING_WAIT: &str = "pending.wait";
    pub const PENDING_CANCEL: &str = "pending.cancel";
    pub const SYNC_STATUS: &str = "sync.status";
    pub const SYNC_NOW: &str = "sync.now";
    pub const SEARCH: &str = "search";

    /// Every user operation, in the order of the specs' tables.
    pub const ALL: [&str; 15] = [
        STATUS,
        ACCOUNTS_LIST,
        PRESETS_LIST,
        PRESET_FETCH,
        ACCOUNT_ADD_BEGIN,
        ACCOUNT_LOGIN_BEGIN,
        CONSENT_BEGIN,
        ACCOUNT_REMOVE,
        SECRETS_CONFIGURE,
        GUIDE_PROVIDER,
        PENDING_WAIT,
        PENDING_CANCEL,
        SYNC_STATUS,
        SYNC_NOW,
        SEARCH,
    ];

    /// The daemon's own methods: the handshake and its lifecycle. Not user operations.
    pub const HELLO: &str = "hello";
    pub const DAEMON_INFO: &str = "daemon.info";
    pub const DAEMON_STOP: &str = "daemon.stop";

    /// The notification the daemon sends to the connection that began a pending operation.
    pub const PENDING_FINISHED: &str = "pending.finished";

    /// Sent, to connections that asked with `sync.now { watch: true }`, on every status change.
    pub const SYNC_PROGRESS: &str = "sync.progress";
}

/// One operation: its wire name and the types it takes and returns.
pub trait Operation {
    const NAME: &'static str;
    type Request: Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static;
    type Response: Serialize + DeserializeOwned + JsonSchema + Send + Sync + 'static;
}

macro_rules! operation {
    ($(#[$doc:meta])* $marker:ident, $name:expr, $request:ty, $response:ty) => {
        $(#[$doc])*
        pub struct $marker;

        impl Operation for $marker {
            const NAME: &'static str = $name;
            type Request = $request;
            type Response = $response;
        }
    };
}

operation!(
    /// Everything `rail doctor` reports, plus the daemon's own state.
    Status, names::STATUS, StatusRequest, StatusResponse
);
operation!(
    /// Configured accounts, token presence and `ai_send` policy.
    AccountsList, names::ACCOUNTS_LIST, AccountsListRequest, AccountsListResponse
);
operation!(
    /// Presets in the known directories.
    PresetsList, names::PRESETS_LIST, PresetsListRequest, PresetsListResponse
);
operation!(
    /// One preset from an `https` URL or a file.
    PresetFetch, names::PRESET_FETCH, PresetFetchRequest, PresetFetchResponse
);
operation!(
    /// Validate a new account, start its login and return the URL; persists on completion.
    AccountAddBegin, names::ACCOUNT_ADD_BEGIN, AccountAddRequest, BeginResponse
);
operation!(
    /// Log in again to an account already in `config.toml`.
    AccountLoginBegin, names::ACCOUNT_LOGIN_BEGIN, LoginBeginRequest, BeginResponse
);
operation!(
    /// Microsoft admin consent for one concrete tenant.
    ConsentBegin, names::CONSENT_BEGIN, ConsentBeginRequest, BeginResponse
);
operation!(
    /// Remove an account's config entry and tokens; its mail rows stay.
    AccountRemove, names::ACCOUNT_REMOVE, AccountRemoveRequest, AccountRemoveResponse
);
operation!(
    /// Choose the token backend and optionally move existing tokens.
    SecretsConfigure, names::SECRETS_CONFIGURE, SecretsConfigureRequest, SecretsConfigureResponse
);
operation!(
    /// Step-by-step provider setup for an agent.
    GuideProvider, names::GUIDE_PROVIDER, GuideRequest, GuideResponse
);
operation!(
    /// Wait, at most five minutes per call, for a begun login or consent.
    PendingWait, names::PENDING_WAIT, WaitRequest, WaitResponse
);
operation!(
    /// Cancel a begun login or consent. A save in progress is never cancelled.
    PendingCancel, names::PENDING_CANCEL, CancelRequest, WaitResponse
);
operation!(
    /// Every account's sync status, or one account's.
    SyncStatus, names::SYNC_STATUS, SyncStatusRequest, SyncStatusResponse
);
operation!(
    /// Wake one account's sync, or every account's.
    SyncNow, names::SYNC_NOW, SyncNowRequest, SyncNowResponse
);
operation!(
    /// Search the store and the provider's whole mailbox.
    Search, names::SEARCH, SearchRequest, SearchResponse
);
operation!(
    /// The handshake: protocol and Rail versions.
    Hello, names::HELLO, HelloRequest, HelloResponse
);
operation!(
    /// The daemon's pid, version, socket and load, without touching config or store.
    DaemonInfoOp, names::DAEMON_INFO, DaemonInfoRequest, DaemonInfo
);
operation!(
    /// Ask the daemon to exit once running saves finish.
    DaemonStop, names::DAEMON_STOP, DaemonStopRequest, DaemonStopResponse
);
