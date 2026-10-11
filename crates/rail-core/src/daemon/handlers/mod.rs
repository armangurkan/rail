//! Every operation the daemon serves.

mod actions;
mod body;
mod guide;
mod login;
mod pending;
mod presets;
mod remove;
mod secrets;
mod status;
mod sync;

use super::registry::Registry;
use crate::ops::op;

/// The registry with every handler.
pub(crate) fn registry() -> Registry {
    let mut registry = Registry::default();
    registry.register::<op::Hello, _, _>(status::hello);
    registry.register::<op::DaemonInfoOp, _, _>(status::daemon_info);
    registry.register::<op::DaemonStop, _, _>(status::daemon_stop);
    registry.register::<op::Status, _, _>(status::status);
    registry.register::<op::AccountsList, _, _>(status::accounts_list);
    registry.register::<op::SecretsConfigure, _, _>(secrets::configure);
    registry.register::<op::AccountAddBegin, _, _>(login::add_begin);
    registry.register::<op::AccountLoginBegin, _, _>(login::login_begin);
    registry.register::<op::ConsentBegin, _, _>(login::consent_begin);
    registry.register::<op::PendingWait, _, _>(pending::wait);
    registry.register::<op::PendingCancel, _, _>(pending::cancel);
    registry.register::<op::PresetsList, _, _>(presets::presets_list);
    registry.register::<op::PresetFetch, _, _>(presets::preset_fetch);
    registry.register::<op::AccountRemove, _, _>(remove::remove);
    registry.register::<op::GuideProvider, _, _>(guide::provider);
    registry.register::<op::SyncStatus, _, _>(sync::status);
    registry.register::<op::SyncNow, _, _>(sync::now);
    registry.register::<op::Search, _, _>(sync::search);
    registry.register::<op::BodyFetch, _, _>(body::fetch);
    registry.register::<op::ActionApply, _, _>(actions::apply);
    registry.register::<op::ActionUndo, _, _>(actions::undo);
    registry.register::<op::ActionRetry, _, _>(actions::retry);
    registry
}
