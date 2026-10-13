//! Microsoft 365 in the mail screen: its thread actions and its writing go through like
//! Gmail's, `l` is refused with `M365_LABELS` (Outlook has folders, not labels), the trash
//! prompt names each provider's trash, and what writing says names the account's provider.

use rail_core::config::AccountKind;
use rail_core::ops::M365_LABELS;

use super::MailScreen;
use super::selection::Picked;

impl MailScreen {
    /// The kind of the account with this store id.
    pub(super) fn kind_of(&self, account_id: i64) -> Option<AccountKind> {
        self.account_index_of(account_id)
            .and_then(|index| self.accounts.get(index))
            .map(|a| a.kind)
    }

    /// True when the account with this store id is a Microsoft 365 account.
    pub(super) fn is_m365(&self, account_id: i64) -> bool {
        self.kind_of(account_id) == Some(AccountKind::M365)
    }

    /// The kind of the configured account at `index` (Gmail when there is none).
    pub(super) fn kind_at(&self, index: usize) -> AccountKind {
        self.accounts
            .get(index)
            .map_or(AccountKind::Gmail, |a| a.kind)
    }

    /// The kind of the account the open message is written from (Gmail with none open).
    pub(super) fn compose_kind(&self) -> AccountKind {
        self.compose.as_ref().map_or(AccountKind::Gmail, |c| {
            self.kind_at(c.composing().account_index)
        })
    }

    /// The kind of the account named `name` (Gmail when it is not configured).
    pub(super) fn kind_named(&self, name: &str) -> AccountKind {
        self.accounts
            .iter()
            .find(|a| a.name == name)
            .map_or(AccountKind::Gmail, |a| a.kind)
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
