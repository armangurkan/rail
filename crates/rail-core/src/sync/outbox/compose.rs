//! From a draft row to an `Outgoing`: addresses, headers, and what still has to be read
//! (files on disk, parts of stored messages) before the message can be built.

use crate::provider::{Mailbox, Outgoing, OutgoingAttachment, Provider, ProviderError};
use crate::store::drafts::{Attachment, Draft, Recipient};
use crate::store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileRef {
    pub path: String,
    pub name: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PartRef {
    pub message_remote: String,
    pub part: String,
    pub name: String,
    pub mime: String,
}

/// The message without its attachment bytes, plus where to get them.
pub(super) struct Assembly {
    pub outgoing: Outgoing,
    pub files: Vec<FileRef>,
    pub parts: Vec<PartRef>,
    /// The name of a `Part` attachment whose message is no longer in the store.
    pub part_gone: Option<String>,
}

fn mailboxes(recipients: &[Recipient]) -> Vec<Mailbox> {
    recipients
        .iter()
        .map(|r| Mailbox {
            name: r.name.clone(),
            address: r.address.clone(),
        })
        .collect()
}

/// Where each attachment's bytes come from: files on disk, and parts by their message's
/// provider id; the name of a part whose message left the store.
type Listed = (Vec<FileRef>, Vec<PartRef>, Option<String>);

fn listed(
    store: &Store,
    account_id: i64,
    attachments: &[Attachment],
) -> Result<Listed, StoreError> {
    let (mut files, mut parts, mut part_gone) = (Vec::new(), Vec::new(), None);
    for attachment in attachments {
        match attachment {
            Attachment::File {
                path, name, mime, ..
            } => files.push(FileRef {
                path: path.clone(),
                name: name.clone(),
                mime: mime.clone(),
            }),
            Attachment::Part {
                message,
                part,
                name,
                mime,
                ..
            } => match store.message_remote_id(account_id, *message)? {
                Some(remote) => parts.push(PartRef {
                    message_remote: remote,
                    part: part.clone(),
                    name: name.clone(),
                    mime: mime.clone(),
                }),
                None => part_gone = Some(name.clone()),
            },
        }
    }
    Ok((files, parts, part_gone))
}

/// The draft as an `Outgoing` from `from` (named `from_name`), with its attachments listed.
/// `references` is the header value the draft stores: ids separated by whitespace.
pub(super) fn outgoing_for(
    store: &Store,
    draft: &Draft,
    from: &str,
    from_name: Option<&str>,
) -> Result<Assembly, StoreError> {
    let (files, parts, part_gone) = listed(store, draft.account_id, &draft.attachments)?;
    let references = draft
        .references
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Ok(Assembly {
        outgoing: Outgoing {
            from_name: from_name.map(str::to_string),
            from: from.to_string(),
            to: mailboxes(&draft.to),
            cc: mailboxes(&draft.cc),
            bcc: mailboxes(&draft.bcc),
            subject: draft.subject.clone(),
            message_id: draft.message_id.clone(),
            in_reply_to: draft.in_reply_to.clone(),
            references,
            body: draft.body.clone(),
            attachments: Vec::new(),
        },
        files,
        parts,
        part_gone,
    })
}

/// Read the files (on the blocking pool) and fetch the parts. A file or part that cannot be
/// had is a permanent failure naming it (the user's own file name, shown back to the user).
pub(super) async fn attach(
    assembly: Assembly,
    provider: &dyn Provider,
) -> Result<Outgoing, ProviderError> {
    if let Some(name) = assembly.part_gone {
        return Err(ProviderError::permanent(&format!(
            "the message that \"{name}\" was forwarded from is no longer in the store"
        )));
    }
    let mut outgoing = assembly.outgoing;
    for file in assembly.files {
        let path = file.path.clone();
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path))
            .await
            .map_err(|_| ProviderError::transient("could not read an attachment"))?
            .map_err(|e| {
                ProviderError::permanent(&format!(
                    "could not read the attachment \"{}\"",
                    file.name
                ))
                .with_detail(e.to_string())
            })?;
        outgoing.attachments.push(OutgoingAttachment {
            name: file.name,
            mime: file.mime,
            bytes,
        });
    }
    for part in assembly.parts {
        let bytes = provider
            .attachment(&part.message_remote, &part.part)
            .await
            .map_err(|e| match e {
                ProviderError::NotFound => ProviderError::permanent(&format!(
                    "the attachment \"{}\" is no longer in Gmail",
                    part.name
                )),
                other => other,
            })?;
        outgoing.attachments.push(OutgoingAttachment {
            name: part.name,
            mime: part.mime,
            bytes,
        });
    }
    Ok(outgoing)
}
