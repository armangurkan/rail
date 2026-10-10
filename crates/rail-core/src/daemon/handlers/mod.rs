//! Every operation the daemon serves.

mod login;
mod pending;
mod secrets;
mod status;

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
    registry
}
