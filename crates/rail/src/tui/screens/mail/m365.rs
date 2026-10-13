//! Microsoft 365 in the mail screen: its thread actions go through, `l` is refused with
//! `M365_LABELS` (Outlook has folders, not labels), and every way to start writing from it
//! says `M365_NOT_YET` at once, before anything is queued. The trash prompt names each
//! provider's trash.

use rail_core::config::AccountKind;
use rail_core::ops::{M365_LABELS, M365_NOT_YET};

use super::MailScreen;
use super::selection::Picked;

impl MailScreen {
    /// The kind of the account with this store id.
    fn kind_of(&self, account_id: i64) -> Option<AccountKind> {
        self.account_index_of(account_id)
            .and_then(|index| self.accounts.get(index))
            .map(|a| a.kind)
    }

    /// True when the account with this store id is a Microsoft 365 account.
    pub(super) fn is_m365(&self, account_id: i64) -> bool {
        self.kind_of(account_id) == Some(AccountKind::M365)
    }

    /// True when the configured account at `index` is a Microsoft 365 account.
    pub(super) fn is_m365_at(&self, index: usize) -> bool {
        self.accounts
            .get(index)
            .is_some_and(|a| a.kind == AccountKind::M365)
    }

    /// Say the writing refusal when `refused`; answers `refused`.
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

    /// `l` on targets holding a Microsoft 365 thread: refused whole, with the folder text.
    pub(super) fn refuse_m365_labels(&mut self, targets: &[(i64, Picked)]) -> bool {
        let refused = targets.iter().any(|(_, p)| self.is_m365(p.account_id));
        if refused {
            self.status.notify(M365_LABELS, self.tick);
        }
        refused
    }

    /// The trash prompt's note for each provider among `targets`, in the order first met.
    pub(super) fn trash_notes(&self, targets: &[(i64, Picked)]) -> Vec<String> {
        let mut kinds: Vec<AccountKind> = Vec::new();
        for kind in targets
            .iter()
            .filter_map(|(_, p)| self.kind_of(p.account_id))
        {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds.iter().map(|k| k.trash_note().to_string()).collect()
    }
}
