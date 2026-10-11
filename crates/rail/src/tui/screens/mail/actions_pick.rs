//! The label pickers (`l`, `m`): Task 7 fills this in.

use rail_core::store::view::LabelChoices;

use super::MailScreen;
use super::selection::Picked;
use crate::tui::app::Effect;
use crate::tui::data::ReadError;
use crate::tui::modals::{Command, PickerMode};

#[derive(Debug)]
pub(crate) struct Picking;

impl MailScreen {
    pub(super) fn open_picker(
        &mut self,
        _mode: PickerMode,
        _targets: Vec<(i64, Picked)>,
    ) -> Vec<Effect> {
        self.status
            .notify("Labels arrive in the next change.", self.tick);
        Vec::new()
    }

    pub(super) fn picker_command(&mut self, _command: Command) -> Vec<Effect> {
        Vec::new()
    }

    pub(super) fn labels_read(
        &mut self,
        _seq: u64,
        _result: Result<LabelChoices, ReadError>,
    ) -> Vec<Effect> {
        Vec::new()
    }
}
