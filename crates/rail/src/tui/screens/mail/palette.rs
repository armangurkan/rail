//! The `:` palette's entries: the active view's key actions and the global ones, Rail's own
//! commands, each account and label.

use super::MailScreen;
use crate::tui::data::ScopeChoice;
use crate::tui::keys::Action;
use crate::tui::modals::{PaletteEntry, Target};

impl MailScreen {
    /// Every palette entry: each key action of the active view and the global ones with its
    /// key, `compose`, `drafts`, `errors`, `theme`, `accent`, `hints`, each account and label.
    pub(crate) fn palette_entries(&self) -> Vec<PaletteEntry> {
        let key = |a: Action| self.ctx.keymap.label(a);
        let views = self.views();
        let mut entries: Vec<PaletteEntry> = Action::ALL
            .iter()
            // `compose` below stands for `c` in every view.
            .filter(|&&a| a != Action::ListCompose)
            .filter(|a| a.views().iter().any(|v| views.contains(v)))
            .map(|&a| PaletteEntry {
                text: a.description().to_string(),
                search: format!("{} {}", a.description(), a.id()),
                key: key(a),
                target: Target::Action(a),
            })
            .collect();
        entries.extend(self.command_entries());
        entries.extend(self.accounts.iter().enumerate().map(|(i, a)| PaletteEntry {
            text: format!("Go to {}'s inbox", a.label),
            search: format!(
                "Go to {}'s inbox account {} {} {}",
                a.label, a.label, a.name, a.address
            ),
            key: account_action(i + 1).map(key).unwrap_or_default(),
            target: Target::Scope(ScopeChoice::Account(i)),
        }));
        entries.extend(self.label_entries());
        entries
    }

    /// Rail's own commands; `compose` carries the key of `c`, `hints` says what it will do.
    fn command_entries(&self) -> Vec<PaletteEntry> {
        let compose = self.ctx.keymap.label(Action::ListCompose);
        let hints = if self.hints {
            "Turn the key hints off"
        } else {
            "Turn the key hints on"
        };
        let commands = [
            ("Write a new message", "compose", compose, Target::Compose),
            ("Open a draft", "drafts", String::new(), Target::Drafts),
            (
                "Show recent errors",
                "errors",
                String::new(),
                Target::Errors,
            ),
            (
                "Change the colour theme",
                "theme",
                String::new(),
                Target::Theme,
            ),
            (
                "Change the accent colour",
                "accent",
                String::new(),
                Target::Accent,
            ),
            (hints, "hints", String::new(), Target::Hints),
        ];
        commands
            .into_iter()
            .map(|(text, id, key, target)| PaletteEntry {
                text: text.into(),
                search: format!("{id} {text}"),
                key,
                target,
            })
            .collect()
    }

    fn label_entries(&self) -> impl Iterator<Item = PaletteEntry> + '_ {
        self.sidebar
            .items()
            .iter()
            .filter(|i| i.depth > 0)
            .map(|item| {
                let account = item
                    .account
                    .and_then(|i| self.accounts.get(i))
                    .map_or("", |a| a.label.as_str());
                PaletteEntry {
                    text: format!("Label: {account} · {}", item.label),
                    search: format!("label {account} {}", item.label),
                    key: String::new(),
                    target: Target::Scope(item.scope.clone()),
                }
            })
    }
}

/// The `list.account_n` action for account number `n` (1 to 9).
fn account_action(n: usize) -> Option<Action> {
    let action = match n {
        1 => Action::ListAccount1,
        2 => Action::ListAccount2,
        3 => Action::ListAccount3,
        4 => Action::ListAccount4,
        5 => Action::ListAccount5,
        6 => Action::ListAccount6,
        7 => Action::ListAccount7,
        8 => Action::ListAccount8,
        9 => Action::ListAccount9,
        _ => return None,
    };
    Some(action)
}

/// The account number of a `list.account_n` action.
pub(super) fn account_number(action: Action) -> Option<usize> {
    let number = match action {
        Action::ListAccount1 => 1,
        Action::ListAccount2 => 2,
        Action::ListAccount3 => 3,
        Action::ListAccount4 => 4,
        Action::ListAccount5 => 5,
        Action::ListAccount6 => 6,
        Action::ListAccount7 => 7,
        Action::ListAccount8 => 8,
        Action::ListAccount9 => 9,
        _ => return None,
    };
    Some(number)
}
