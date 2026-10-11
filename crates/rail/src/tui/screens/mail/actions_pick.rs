//! The label pickers (`l`, `m`): refused for a selection across accounts, otherwise a store
//! read of the account's labels and what the threads carry, then the picker; its answer is a
//! `labels` or `move` action with grace.

use std::collections::BTreeSet;

use rail_core::ops::Action as MailAction;
use rail_core::store::view::LabelChoices;

use super::MailScreen;
use super::selection::Picked;
use crate::tui::app::Effect;
use crate::tui::data::ReadError;
use crate::tui::modals::{Command, Modal, Picker, PickerItem, PickerMode};

/// A picker waiting for its labels, and what it applies to.
#[derive(Debug)]
pub(crate) struct Picking {
    seq: u64,
    mode: PickerMode,
    targets: Vec<(i64, Picked)>,
}

impl MailScreen {
    pub(super) fn open_picker(
        &mut self,
        mode: PickerMode,
        targets: Vec<(i64, Picked)>,
    ) -> Vec<Effect> {
        let accounts: BTreeSet<i64> = targets.iter().map(|(_, p)| p.account_id).collect();
        let Some(account_id) = accounts.first().copied().filter(|_| accounts.len() == 1) else {
            self.status.notify(
                "Labels belong to one account: select threads from one account.",
                self.tick,
            );
            return Vec::new();
        };
        let seq = self.next_seq();
        let threads = targets.iter().map(|(id, _)| *id).collect();
        self.actions.picking = Some(Picking { seq, mode, targets });
        vec![self.ctx.store.labels(seq, account_id, threads)]
    }

    pub(super) fn labels_read(
        &mut self,
        seq: u64,
        result: Result<LabelChoices, ReadError>,
    ) -> Vec<Effect> {
        let Some(mode) = self
            .actions
            .picking
            .as_ref()
            .filter(|p| p.seq == seq)
            .map(|p| p.mode)
        else {
            return Vec::new();
        };
        match result {
            Ok(choices) if choices.labels.is_empty() => {
                self.actions.picking = None;
                self.status
                    .notify("This account has no labels to choose from.", self.tick);
            }
            Ok(choices) => {
                let items = choices
                    .labels
                    .into_iter()
                    .map(|l| PickerItem {
                        checked: mode == PickerMode::Toggle
                            && choices.current.contains(&l.folder_id),
                        folder_id: l.folder_id,
                        name: l.name,
                    })
                    .collect();
                self.modal = Some(Modal::Picker(Picker::new(mode, items)));
            }
            Err(error) => {
                self.actions.picking = None;
                self.side_read_failed(error, "labels");
            }
        }
        Vec::new()
    }

    pub(super) fn picker_command(&mut self, command: Command) -> Vec<Effect> {
        let Some(picking) = self.actions.picking.take() else {
            return Vec::new();
        };
        let action = match command {
            Command::Labels { add, remove } if add.is_empty() && remove.is_empty() => {
                self.status.notify("No labels changed.", self.tick);
                return Vec::new();
            }
            Command::Labels { add, remove } => MailAction::Labels { add, remove },
            Command::MoveTo(to) => MailAction::Move { to },
            _ => return Vec::new(),
        };
        self.send_action(action, &picking.targets, true)
    }
}
