//! Microsoft 365 in Part A: its threads read, and opening one marks it read; nothing else. The
//! action keys, and every way to start writing from it, say `M365_NOT_YET` at once, before
//! anything is queued.

use rail_core::config::AccountKind;
use rail_core::ops::M365_NOT_YET;

use super::MailScreen;

impl MailScreen {
    /// True when the account with this store id is a Microsoft 365 account.
    pub(super) fn is_m365(&self, account_id: i64) -> bool {
        self.account_index_of(account_id)
            .is_some_and(|index| self.is_m365_at(index))
    }

    /// True when the configured account at `index` is a Microsoft 365 account.
    pub(super) fn is_m365_at(&self, index: usize) -> bool {
        self.accounts
            .get(index)
            .is_some_and(|a| a.kind == AccountKind::M365)
    }

    /// Say Part A's refusal when `refused`; answers `refused`.
    pub(super) fn refuse_m365(&mut self, refused: bool) -> bool {
        if refused {
            self.status.notify(M365_NOT_YET, self.tick);
        }
        refused
    }

    /// The backstop of the review's save and send: refuse when the message being written is
    /// from a Microsoft 365 account.
    pub(super) fn refuse_m365_compose(&mut self) -> bool {
        let refused = self
            .compose
            .as_ref()
            .is_some_and(|c| self.is_m365_at(c.composing().account_index));
        self.refuse_m365(refused)
    }
}
