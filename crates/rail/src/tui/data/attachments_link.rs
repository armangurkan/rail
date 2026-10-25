//! The two attachment calls the mail screen sends: `attachment.save` and `attachment.open`.
//! Both answer a **path**, never bytes, so nothing here carries an attachment. They live
//! beside `daemon_link.rs` rather than in it because that file is already near
//! CONTRIBUTING's 800-line cap.

use rail_core::ops::AttachmentSaveRequest;
use rail_core::ops::op::AttachmentSave;

use super::daemon_link::Link;
use super::{DaemonLink, MailEvent, job, send};
use crate::tui::app::Effect;

impl DaemonLink {
    /// Save `part_ids` of `message_id` into `dir`; an empty list means every attachment the
    /// store lists for that message. `dir` is absolute: the daemon's working directory is `/`,
    /// so the client resolves it.
    pub fn attachment_save(
        &self,
        account: String,
        message_id: String,
        part_ids: Vec<String>,
        dir: String,
    ) -> Effect {
        // One part asked for: the status line can then name the file it was.
        let single = part_ids.len() == 1;
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = AttachmentSaveRequest {
                    account: account.clone(),
                    message_id,
                    part_ids,
                    dir: dir.clone(),
                };
                let result = connector.call::<AttachmentSave>(&request).await;
                send(
                    &sink,
                    MailEvent::AttachmentSaved {
                        result,
                        account,
                        dir,
                        single,
                    },
                );
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move {
                fake.attachment_save(&sink, account, message_id, part_ids, dir, single);
            }),
        }
    }
}

#[cfg(test)]
impl super::FakeDaemon {
    /// Record the call and answer it: `save_answer` when a test set one, else every requested
    /// part `Saved` into `dir` under its own name.
    fn attachment_save(
        &self,
        sink: &crate::tui::app::TaskSink,
        account: String,
        message_id: String,
        part_ids: Vec<String>,
        dir: String,
        single: bool,
    ) {
        use rail_core::ops::{AttachmentOutcome, AttachmentResult, AttachmentSaveResponse};
        self.record(super::FakeCall::AttachmentSave {
            account: account.clone(),
            message_id,
            part_ids: part_ids.clone(),
            dir: dir.clone(),
        });
        let result = self.save_answer.lock().unwrap().take().unwrap_or_else(|| {
            let results = part_ids
                .iter()
                .map(|part_id| AttachmentResult {
                    part_id: part_id.clone(),
                    outcome: AttachmentOutcome::Saved {
                        path: std::path::Path::new(&dir)
                            .join(part_id)
                            .display()
                            .to_string(),
                        size: 0,
                    },
                })
                .collect();
            Ok(AttachmentSaveResponse { results })
        });
        send(
            sink,
            MailEvent::AttachmentSaved {
                result,
                account,
                dir,
                single,
            },
        );
    }
}
