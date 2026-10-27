//! The two attachment calls the mail screen sends: `attachment.save` and `attachment.open`.
//! Both answer a **path**, never bytes, so nothing here carries an attachment. They live
//! beside `daemon_link.rs` rather than in it because that file is already near
//! CONTRIBUTING's 800-line cap.

use rail_core::ops::op::{AttachmentOpen, AttachmentSave};
use rail_core::ops::{AttachmentOpenRequest, AttachmentSaveRequest};

use super::daemon_link::Link;
use super::{DaemonLink, MailEvent, job, send};
use crate::tui::app::Effect;

impl DaemonLink {
    /// Save `part_ids` of `message_id` into `dir`; an empty list means every attachment the
    /// store lists for that message. `dir` is absolute: the daemon's working directory is `/`,
    /// so the client resolves it. `name` is the one file this save asked for, when it asked for
    /// one, and `unopenable` the type an `Enter` would not open when that turned into this
    /// save: both are carried here and handed back with the answer, so the status line of a
    /// save that is still in flight cannot be spoken for by another save's answer.
    pub fn attachment_save(
        &self,
        account: String,
        message_id: String,
        part_ids: Vec<String>,
        dir: String,
        name: Option<String>,
        unopenable: Option<String>,
    ) -> Effect {
        // One part asked for: the log line says which kind of save this was.
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
                        name,
                        unopenable,
                    },
                );
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move {
                let result = fake.attachment_save(&account, message_id, &part_ids, &dir);
                send(
                    &sink,
                    MailEvent::AttachmentSaved {
                        result,
                        account,
                        dir,
                        single,
                        name,
                        unopenable,
                    },
                );
            }),
        }
    }

    /// Write `part_id` of `message_id` into a private directory under the cache and answer its
    /// path, for the system opener. `filename` is carried so a failure's line can name the
    /// part; it never reaches the daemon, which takes the name from the stored type.
    pub fn attachment_open(
        &self,
        account: String,
        message_id: String,
        part_id: String,
        filename: String,
    ) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = AttachmentOpenRequest {
                    account: account.clone(),
                    message_id,
                    part_id,
                };
                let result = connector.call::<AttachmentOpen>(&request).await;
                send(
                    &sink,
                    MailEvent::AttachmentOpened {
                        result,
                        account,
                        filename,
                    },
                );
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move {
                fake.attachment_open(&sink, account, message_id, part_id, filename);
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
        account: &str,
        message_id: String,
        part_ids: &[String],
        dir: &str,
    ) -> Result<rail_core::ops::AttachmentSaveResponse, rail_core::ops::OpError> {
        use rail_core::ops::{AttachmentOutcome, AttachmentResult, AttachmentSaveResponse};
        self.record(super::FakeCall::AttachmentSave {
            account: account.to_string(),
            message_id,
            part_ids: part_ids.to_vec(),
            dir: dir.to_string(),
        });
        self.save_answer.lock().unwrap().take().unwrap_or_else(|| {
            let results = part_ids
                .iter()
                .map(|part_id| AttachmentResult {
                    part_id: part_id.clone(),
                    outcome: AttachmentOutcome::Saved {
                        path: std::path::Path::new(dir)
                            .join(part_id)
                            .display()
                            .to_string(),
                        size: 0,
                    },
                })
                .collect();
            Ok(AttachmentSaveResponse { results })
        })
    }
}

#[cfg(test)]
impl super::FakeDaemon {
    /// Record the call and answer it: `open_answer` when a test set one, else a fresh
    /// `<open_dir>/fake-<n>/<filename>`, written as an empty 0600 file in a 0700 directory
    /// exactly as the daemon would, so a test can watch it be removed. Without `open_dir`
    /// the fake cannot guess the cache, and says so.
    fn attachment_open(
        &self,
        sink: &crate::tui::app::TaskSink,
        account: String,
        message_id: String,
        part_id: String,
        filename: String,
    ) {
        self.record(super::FakeCall::AttachmentOpen {
            account: account.clone(),
            message_id,
            part_id,
        });
        let result = self
            .open_answer
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| self.fake_copy(&filename));
        send(
            sink,
            MailEvent::AttachmentOpened {
                result,
                account,
                filename,
            },
        );
    }

    /// The copy the default answer writes: a token directory of its own, and `filename` in it.
    fn fake_copy(
        &self,
        filename: &str,
    ) -> Result<rail_core::ops::AttachmentOpenResponse, rail_core::ops::OpError> {
        use rail_core::ops::{AttachmentOpenResponse, ErrorCode, OpError};
        let Some(root) = self.open_dir.lock().unwrap().clone() else {
            return Err(OpError::internal("open_dir unset"));
        };
        let token = self
            .next_token
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = root.join(format!("fake-{token}"));
        let io = |what: &str, error: &std::io::Error| OpError::io(ErrorCode::Internal, what, error);
        rail_core::paths::create_private_dir(&dir)
            .map_err(|error| io("creating a fake token directory", &error))?;
        let path = dir.join(filename);
        rail_core::paths::write_private_atomic(&path, b"")
            .map_err(|error| io("writing a fake opened attachment", &error))?;
        Ok(AttachmentOpenResponse {
            path: path.display().to_string(),
        })
    }
}
