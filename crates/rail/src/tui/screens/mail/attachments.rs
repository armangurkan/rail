//! The screen side of saving a thread's attachments: the modal's `s` and `S`, the reader's
//! `w`, and the status line each answered save gets. `reading.rs` already runs to 600 lines
//! against CONTRIBUTING's 800-line cap, so this side lives here.
//!
//! Every filename, directory and type drawn on the status line goes through `text::escape`:
//! all three are the sender's or the user's own text. The stored line is **unfitted** — the
//! hint bar fits it to its room when it draws.

use std::path::Path;

use rail_core::config::AccountKind;
use rail_core::ops::{AttachmentOutcome, AttachmentResult, AttachmentSaveResponse, OpError};
use rail_core::provider::graph::REFERENCE_ATTACHMENT;

use super::MailScreen;
use crate::tui::app::Effect;
use crate::tui::modals::AttachmentTarget;
use crate::tui::text::escape;

/// What the attachment keys are waiting on.
#[derive(Default)]
pub(super) struct AttachmentState {
    /// The file the save in flight named, when one part was asked for: a failure's line names
    /// it, and a whole message's save has no one file to name.
    name: Option<String>,
    /// The type `Enter` would not open, when it turned into a save (FR-6).
    unopenable: Option<String>,
}

impl MailScreen {
    /// The list's `s`: save the highlighted part alone. `unopenable` is set when an `Enter`
    /// on a type Rail does not open became this save.
    pub(super) fn save_attachment(
        &mut self,
        target: AttachmentTarget,
        unopenable: Option<String>,
    ) -> Vec<Effect> {
        self.send_save(
            &target.account,
            &target.message,
            vec![target.part_id],
            Some(target.filename),
            unopenable,
        )
    }

    /// The list's `S`: save every attachment of the highlighted row's message, not of the
    /// thread.
    pub(super) fn save_all_attachments(&mut self, target: AttachmentTarget) -> Vec<Effect> {
        self.send_save(&target.account, &target.message, Vec::new(), None, None)
    }

    /// The reader's `w`: save the cursor message's attachments without opening the list. A
    /// line that belongs to no message, and a message with no attachments, each say so and
    /// send nothing.
    pub(super) fn save_cursor_attachments(&mut self) -> Vec<Effect> {
        let Some(reader) = self.reader.as_ref() else {
            return Vec::new();
        };
        let account_id = reader.detail().account_id;
        let found = reader
            .message_at_cursor()
            .map(|m| (m.remote_id.clone(), m.attachments.is_empty()));
        match found {
            None => {
                self.status.notify("Move onto a message first.", self.tick);
                Vec::new()
            }
            Some((_, true)) => {
                self.status
                    .notify("This message has no attachments.", self.tick);
                Vec::new()
            }
            Some((remote, false)) => {
                let account = self.account_name(account_id).unwrap_or_default();
                self.send_save(&account, &remote, Vec::new(), None, None)
            }
        }
    }

    /// Send one `attachment.save` and say it is running. An empty `part_ids` means every
    /// attachment the store lists for that message.
    fn send_save(
        &mut self,
        account: &str,
        message: &str,
        part_ids: Vec<String>,
        name: Option<String>,
        unopenable: Option<String>,
    ) -> Vec<Effect> {
        let Some(dir) = self
            .ctx
            .attachment_dir
            .as_deref()
            .and_then(Path::to_str)
            .map(ToString::to_string)
        else {
            self.no_destination();
            return Vec::new();
        };
        self.attachment.name = name;
        self.attachment.unopenable = unopenable;
        self.status.notify("Saving…", self.tick);
        vec![
            self.ctx
                .link
                .attachment_save(account.to_string(), message.to_string(), part_ids, dir),
        ]
    }

    /// Neither `[general] attachment_dir` nor a download directory resolved, so there is
    /// nowhere to save to and nothing is sent (FR-17).
    pub(super) fn no_destination(&mut self) {
        let text = "Set [general] attachment_dir: Rail could not find your download directory."
            .to_string();
        self.status.error(text, self.ctx.clock.now(), self.tick);
    }

    /// `attachment.save` answered: one status line, per §8's table.
    pub(super) fn attachment_saved(
        &mut self,
        result: Result<AttachmentSaveResponse, OpError>,
        account: &str,
        dir: &str,
        single: bool,
    ) -> Vec<Effect> {
        // Taken either way, so a later answer never reuses this one's file.
        let name = self.attachment.name.take().filter(|_| single);
        let unopenable = self.attachment.unopenable.take();
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(code = ?error.code, "an attachment save failed");
                let text = format!("Could not save the attachments: {}", escape(&error.message));
                self.status.error(text, self.ctx.clock.now(), self.tick);
                return Vec::new();
            }
        };
        if response.results.is_empty() {
            self.status
                .notify("This message has no attachments.", self.tick);
            return Vec::new();
        }
        let (text, failed) = self.save_line(
            &response.results,
            account,
            dir,
            name.as_deref(),
            unopenable.as_deref(),
        );
        if failed {
            tracing::warn!(
                parts = response.results.len(),
                "an attachment save did not finish"
            );
            self.status.error(text, self.ctx.clock.now(), self.tick);
        } else {
            self.status.notify(text, self.tick);
        }
        Vec::new()
    }

    /// The line one answered save gets, and whether it is an error.
    fn save_line(
        &self,
        results: &[AttachmentResult],
        account: &str,
        dir: &str,
        name: Option<&str>,
        unopenable: Option<&str>,
    ) -> (String, bool) {
        let total = results.len();
        let saved = results.iter().filter(|r| written(r).is_some()).count();
        let dir = escape(dir);
        if saved == total {
            if total > 1 {
                return (format!("Saved {total} attachments to {dir}"), false);
            }
            // The name the file actually got, so a clash that became `2-plan.pdf` says so.
            let file = escape(written(&results[0]).unwrap_or_default());
            let text = match unopenable {
                Some(mime) => format!(
                    "Rail does not open {} files; saved {file} to {dir}",
                    escape(mime)
                ),
                None => format!("Saved {file} to {dir}"),
            };
            return (text, false);
        }
        if total == 1 {
            return (self.part_failure(&results[0].outcome, account, name), true);
        }
        let first = results
            .iter()
            .find(|r| written(r).is_none())
            .map(|r| self.outcome_text(&r.outcome, account))
            .unwrap_or_default();
        let failed = total - saved;
        (
            format!("Saved {saved} of {total} attachments to {dir}; {failed} failed: {first}"),
            true,
        )
    }

    /// The only requested part did not land: its own line. A provider's whole sentence (a
    /// Microsoft 365 reference attachment names where to reach the file) is passed through
    /// as it is; anything else names the file that was asked for.
    fn part_failure(
        &self,
        outcome: &AttachmentOutcome,
        account: &str,
        name: Option<&str>,
    ) -> String {
        match outcome {
            AttachmentOutcome::Failed(error) if error.message != REFERENCE_ATTACHMENT => {
                let message = escape(&error.message);
                match name {
                    Some(file) => format!("Could not save {}: {message}", escape(file)),
                    None => format!("Could not save the attachment: {message}"),
                }
            }
            other => self.outcome_text(other, account),
        }
    }

    /// What one outcome says on its own: the provider no longer has the part, or the
    /// provider's own words.
    fn outcome_text(&self, outcome: &AttachmentOutcome, account: &str) -> String {
        match outcome {
            AttachmentOutcome::Failed(error) => escape(&error.message),
            _ => format!(
                "This attachment is no longer in {}.",
                self.provider_of(account)
            ),
        }
    }

    /// The provider of the account with this name, as Rail's messages call it.
    pub(super) fn provider_of(&self, account: &str) -> &'static str {
        self.accounts
            .iter()
            .find(|entry| entry.name == account)
            .map_or(AccountKind::Gmail, |entry| entry.kind)
            .provider_name()
    }
}

/// The file name a `Saved` outcome's path ends in; `None` for a part that did not land.
fn written(result: &AttachmentResult) -> Option<&str> {
    let AttachmentOutcome::Saved { path, .. } = &result.outcome else {
        return None;
    };
    Path::new(path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
}
