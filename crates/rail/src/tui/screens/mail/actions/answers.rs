//! The answers to the actions' calls: an `action.apply` keeps its rows for `z` (or undoes them at
//! once when `z` came first) and a failed one puts its selected threads back; an `action.undo`
//! says what came back. A record a newer action replaced still takes its calls' answers here.

use std::collections::HashSet;

use rail_core::ops::{
    ActionApplyResponse, ActionUndoResponse, ErrorCode, NOT_IN_INBOX, OpError, OutboxId, RowToken,
};

use super::{LastAction, Record, verbs};
use crate::tui::app::Effect;
use crate::tui::data::ApplyCall;
use crate::tui::keys::Action;
use crate::tui::screens::mail::MailScreen;
use crate::tui::screens::mail::selection::{Picked, Selection};

/// What one `action.undo` answered. Both can hold at once over a bulk action's chunks.
#[derive(Debug, Clone, Copy, Default)]
struct Undone {
    /// Some rows were already sent, being sent, failed or unknown.
    too_late: bool,
    /// Some rows came back.
    some: bool,
}

/// What a failed call could not do: a snooze's archive fails as the snooze.
fn failed_verb(call: &ApplyCall) -> &'static str {
    if call.snooze.is_some() {
        "snooze"
    } else {
        verbs(&call.action).1
    }
}

/// The rows one call queued, each under that call's token.
fn row_tokens(call: &ApplyCall, rows: Vec<OutboxId>) -> Vec<RowToken> {
    rows.into_iter()
        .map(|row| RowToken {
            row,
            client_token: call.client_token.clone(),
        })
        .collect()
}

/// A snooze's call refused because none of its threads had mail in the inbox: they count as
/// left out while the snooze's other calls may still take theirs.
fn left_out(call: &ApplyCall, error: &OpError) -> bool {
    call.snooze.is_some() && error.code == ErrorCode::InvalidInput && error.message == NOT_IN_INBOX
}

/// A failed call's selected `threads` come back to the selection, if nothing was selected
/// since (or an earlier failed call of the record already put its threads back).
fn put_back(record: &mut LastAction, selection: &mut Selection, threads: &[i64]) {
    if record.restored || selection.is_empty() {
        record.restored = true;
        let threads: HashSet<i64> = threads.iter().copied().collect();
        let back: Vec<(i64, Picked)> = record
            .selected
            .iter()
            .filter(|(id, _)| threads.contains(id))
            .copied()
            .collect();
        selection.restore(&back);
    }
}

impl MailScreen {
    pub(super) fn applied(
        &mut self,
        call: &ApplyCall,
        result: Result<ActionApplyResponse, OpError>,
    ) -> Vec<Effect> {
        let tracked = call.grace
            && self
                .actions
                .last
                .as_ref()
                .is_some_and(|l| l.seq == call.seq);
        let older = call.grace && !tracked && self.actions.replaced.contains_key(&call.seq);
        let mut effects = Vec::new();
        match result {
            Ok(answer) if tracked => effects = self.call_answered(call, answer),
            Ok(answer) if older => effects = self.older_answered(call, answer.rows),
            Ok(_) => {}
            Err(error) if tracked && left_out(call, &error) => self.call_left_out(call, error),
            Err(error) => {
                self.action_error(failed_verb(call), &error);
                if tracked {
                    self.call_failed(call);
                } else if older {
                    self.older_failed(call);
                }
            }
        }
        effects.extend(self.dirty());
        effects
    }

    /// One account's `action.apply` answered: keep its rows for `z`, or undo them at once if
    /// `z` was pressed first.
    fn call_answered(&mut self, call: &ApplyCall, answer: ActionApplyResponse) -> Vec<Effect> {
        let Some(last) = self.actions.last.as_mut() else {
            return Vec::new();
        };
        let rows = row_tokens(call, answer.rows);
        last.pending = last.pending.saturating_sub(1);
        if call.snooze.is_some() {
            last.snoozed += answer.applied;
            last.left_out += call.threads.len().saturating_sub(answer.applied);
        }
        if last.undo_wanted {
            if rows.is_empty() {
                self.finish_undo(true);
                return Vec::new();
            }
            let batches = crate::tui::screens::mail::bulk::undo_batches(&rows);
            last.undos += batches.len();
            let seq = last.seq;
            return batches
                .into_iter()
                .map(|batch| self.ctx.link.undo(seq, batch))
                .collect();
        }
        last.rows.extend(rows);
        self.say_done(call);
        Vec::new()
    }

    /// The newest action's notice, while `z` can take it back: what it did and how many; a
    /// snooze's says until when, and what its answered calls snoozed and left out so far. Once
    /// one of its calls failed, nothing is said: that error stays on the bar.
    fn say_done(&mut self, call: &ApplyCall) {
        let Some(last) = self.actions.last.as_ref().filter(|l| !l.failed) else {
            return;
        };
        let undoable = !last.rows.is_empty();
        let counts = (last.count, last.snoozed, last.left_out);
        let left = last.until.saturating_sub(self.tick);
        let text = if let Some(until) = call.snooze {
            self.snoozed_notice(until, undoable, counts)
        } else {
            let (done, _) = verbs(&call.action);
            let key = self.ctx.keymap.label(Action::GlobalUndo);
            let count = crate::tui::screens::mail::header::thousands(
                u64::try_from(counts.0).unwrap_or(u64::MAX),
            );
            format!("{done} {count} · {key} undoes")
        };
        self.status.notify_for(text, self.tick, left);
    }

    /// A snooze's call refused because none of its threads had mail in the inbox: they count as
    /// left out. A snooze that took some keeps counting them in its notice; once every call
    /// answered, one that took nothing says the refusal and gives the threads back.
    fn call_left_out(&mut self, call: &ApplyCall, refusal: OpError) {
        let Some(last) = self.actions.last.as_mut() else {
            return;
        };
        last.pending = last.pending.saturating_sub(1);
        last.left_out += call.threads.len();
        last.refused.extend_from_slice(&call.threads);
        last.refusal = Some(refusal);
        if last.undo_wanted {
            self.finish_undo(false);
        } else if last.snoozed > 0 {
            self.say_done(call);
        } else if last.pending == 0 {
            self.nothing_snoozed();
        }
    }

    /// Every call of a snooze answered and none took a thread: the refused threads go back to
    /// the selection and the refusal is said; with nothing to undo, the record goes.
    fn nothing_snoozed(&mut self) {
        let Some(last) = self.actions.last.as_mut() else {
            return;
        };
        let refused = std::mem::take(&mut last.refused);
        put_back(last, &mut self.actions.selection, &refused);
        // An earlier call's error stays on the bar instead.
        let refusal = last.refusal.take().filter(|_| !last.failed);
        if last.rows.is_empty() && last.undos == 0 {
            self.actions.last = None;
        }
        if let Some(refusal) = refusal {
            self.action_error("snooze", &refusal);
        }
    }

    /// One account's `action.apply` failed: its selected threads come back to the selection
    /// (if nothing was selected since), its error stays on the bar while the other calls
    /// answer, and a record with nothing to undo is dropped.
    fn call_failed(&mut self, call: &ApplyCall) {
        let Some(last) = self.actions.last.as_mut() else {
            return;
        };
        last.pending = last.pending.saturating_sub(1);
        last.failed = true;
        put_back(last, &mut self.actions.selection, &call.threads);
        if last.undo_wanted {
            self.finish_undo(false);
        } else if last.pending == 0 && last.rows.is_empty() && last.undos == 0 {
            // A snooze that took nothing gives back its calls refused as not in the inbox too;
            // this call's error stays on the bar.
            let refused = std::mem::take(&mut last.refused);
            put_back(last, &mut self.actions.selection, &refused);
            self.actions.last = None;
        }
    }

    /// A replaced record's call answered: its rows are undone if `z` was pressed for it, else
    /// they stay queued (only the newest action is undoable). Nothing is said: the bar shows
    /// the newer action.
    fn older_answered(&mut self, call: &ApplyCall, rows: Vec<OutboxId>) -> Vec<Effect> {
        let Some(old) = self.actions.replaced.get_mut(&call.seq) else {
            return Vec::new();
        };
        old.pending = old.pending.saturating_sub(1);
        let undo = old.undo_wanted && !rows.is_empty();
        if undo {
            old.undos += 1;
        }
        self.drop_older_if_done(call.seq);
        if !undo {
            return Vec::new();
        }
        vec![self.ctx.link.undo(call.seq, row_tokens(call, rows))]
    }

    /// A replaced record's call failed (its error is on the bar): its selected threads come
    /// back to the selection, as for the newest action.
    fn older_failed(&mut self, call: &ApplyCall) {
        let Some(old) = self.actions.replaced.get_mut(&call.seq) else {
            return;
        };
        old.pending = old.pending.saturating_sub(1);
        put_back(old, &mut self.actions.selection, &call.threads);
        self.drop_older_if_done(call.seq);
    }

    /// An undo of a replaced record answered: only `too late to undo` is said (the bar shows
    /// the newer action otherwise; a failed undo has its error there already). True when the
    /// seq was a replaced record's.
    fn older_undone(&mut self, seq: u64, too_late: bool, failed: bool) -> bool {
        let Some(old) = self.actions.replaced.get_mut(&seq) else {
            return false;
        };
        old.undos = old.undos.saturating_sub(1);
        if too_late && !failed {
            self.status.notify("too late to undo", self.tick);
        }
        self.drop_older_if_done(seq);
        true
    }

    /// A replaced record goes once its calls and undos have all answered.
    fn drop_older_if_done(&mut self, seq: u64) {
        let done = self
            .actions
            .replaced
            .get(&seq)
            .is_some_and(|old| old.pending == 0 && old.undos == 0);
        if done {
            self.actions.replaced.remove(&seq);
        }
    }

    /// Once `z` was pressed and every call and undo of the record is answered, report the
    /// combined result and drop the record. `say_nothing`: say so when nothing was undone
    /// (no error is on the bar to say why). True when a send was undone.
    fn finish_undo(&mut self, say_nothing: bool) -> bool {
        let tick = self.tick;
        let done = self
            .actions
            .last
            .as_ref()
            .is_some_and(|l| l.undo_wanted && l.pending == 0 && l.undos == 0);
        if !done {
            return false;
        }
        let Some(last) = self.actions.last.take() else {
            return false;
        };
        if last.undid {
            // A bulk action's chunks can answer both ways at once: say which it was.
            let text = match (last.too_late, last.some_undone, last.kind) {
                (true, true, _) => "undone in part: some were already sent",
                (true, false, _) => "too late to undo",
                (false, _, Record::Pushed) => "kept on this computer",
                (false, _, Record::Threads | Record::Sent) => "undone",
            };
            self.status.notify(text, tick);
        } else if say_nothing {
            self.status.notify("Nothing to undo.", tick);
        }
        last.kind == Record::Sent && last.undid && !last.too_late
    }

    /// An `action.undo` answered. An answer for a replaced record says only `too late to
    /// undo`; one for no known record is stale and ignored. True when a send was undone.
    fn undone(&mut self, seq: u64, answered: Undone, failed: bool) -> bool {
        if self.older_undone(seq, answered.too_late, failed) {
            return false;
        }
        let Some(last) = self.actions.last.as_mut().filter(|l| l.seq == seq) else {
            return false;
        };
        last.undos = last.undos.saturating_sub(1);
        if !failed {
            last.undid = true;
            last.too_late |= answered.too_late;
            last.some_undone |= answered.some;
        }
        self.finish_undo(false)
    }

    /// An `action.undo` answered: what came back, or its error on the bar. Undoing a send
    /// reopens its review.
    pub(super) fn undo_answered(
        &mut self,
        seq: u64,
        result: Result<ActionUndoResponse, OpError>,
    ) -> Vec<Effect> {
        let (answered, failed) = match result {
            Ok(answer) => (
                Undone {
                    too_late: !answer.too_late.is_empty(),
                    some: !answer.undone.is_empty(),
                },
                false,
            ),
            Err(error) => {
                self.action_error("undo", &error);
                (Undone::default(), true)
            }
        };
        if self.undone(seq, answered, failed) {
            self.reopen_sent();
        }
        self.dirty()
    }
}
