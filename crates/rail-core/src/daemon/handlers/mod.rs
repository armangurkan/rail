//! Every operation the daemon serves.

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
    registry
}
