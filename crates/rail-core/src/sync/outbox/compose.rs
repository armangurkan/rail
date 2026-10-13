//! From a draft row to an `OutgoingMail`: addresses, headers, what the provider needs besides
//! the message (kind, source, its copy of the draft, thread), and what still has to be read
//! (files on disk, parts of stored messages) before the message can go.

use crate::provider::{
    DraftIds, Mailbox, Outgoing, OutgoingAttachment, OutgoingMail, Provider, ProviderError,
};
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
    /// The message and what the provider needs with it; no attachment yet.
    pub mail: OutgoingMail,
    pub files: Vec<FileRef>,
    pub parts: Vec<PartRef>,
    /// A `Part` attachment's message is no longer in the store.
    pub part_gone: bool,
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
/// provider id; whether a part's message left the store.
type Listed = (Vec<FileRef>, Vec<PartRef>, bool);

fn listed(
    store: &Store,
    account_id: i64,
    attachments: &[Attachment],
) -> Result<Listed, StoreError> {
    let (mut files, mut parts, mut part_gone) = (Vec::new(), Vec::new(), false);
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
                None => part_gone = true,
            },
        }
    }
    Ok((files, parts, part_gone))
}

/// The provider's copy of the draft once pushed or imported (Graph: both ids are its id).
fn remote_of(draft: &Draft) -> Option<DraftIds> {
    let draft_id = draft.remote_draft_id.clone()?;
    let message_id = draft
        .remote_message_id
        .clone()
        .unwrap_or_else(|| draft_id.clone());
    Some(DraftIds {
        draft_id,
        message_id,
    })
}

/// The draft's message from `from` (named `from_name`). `references` is the header value the
/// draft stores: ids separated by whitespace.
fn outgoing_of(draft: &Draft, from: &str, from_name: Option<&str>) -> Outgoing {
    let references = draft
        .references
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    Outgoing {
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
    }
}

/// The draft as an `OutgoingMail` from `from` (named `from_name`), with its attachments listed.
pub(super) fn outgoing_for(
    store: &Store,
    draft: &Draft,
    from: &str,
    from_name: Option<&str>,
) -> Result<Assembly, StoreError> {
    let (files, parts, part_gone) = listed(store, draft.account_id, &draft.attachments)?;
    Ok(Assembly {
        mail: OutgoingMail {
            outgoing: outgoing_of(draft, from, from_name),
            kind: draft.kind,
            source: draft.source_remote_id.clone(),
            remote: remote_of(draft),
            thread: draft.remote_thread_id.clone(),
        },
        files,
        parts,
        part_gone,
    })
}

/// One file, read on the blocking pool. The error leaves the name out: it is text from
/// outside Rail, shown without the label that marks such text.
async fn read_file(file: FileRef) -> Result<OutgoingAttachment, ProviderError> {
    let path = file.path.clone();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path))
        .await
        .map_err(|_| ProviderError::transient("could not read an attachment"))?
        .map_err(|e| {
            ProviderError::permanent("could not read an attachment of this message")
                .with_detail(e.to_string())
        })?;
    Ok(OutgoingAttachment {
        name: file.name,
        mime: file.mime,
        bytes,
        carried: None,
    })
}

/// One part of a stored message, fetched from the provider.
async fn fetch_part(
    provider: &dyn Provider,
    part: PartRef,
) -> Result<OutgoingAttachment, ProviderError> {
    let bytes = provider
        .attachment(&part.message_remote, &part.part)
        .await
        .map_err(|e| match e {
            ProviderError::NotFound => {
                ProviderError::permanent("an attachment of this message is no longer in Gmail")
            }
            other => other,
        })?;
    Ok(OutgoingAttachment {
        name: part.name,
        mime: part.mime,
        bytes,
        carried: None,
    })
}

/// Read the files and fetch the parts. A file or part that cannot be had is a permanent
/// failure.
pub(super) async fn attach(
    assembly: Assembly,
    provider: &dyn Provider,
) -> Result<OutgoingMail, ProviderError> {
    if assembly.part_gone {
        return Err(ProviderError::permanent(
            "the message an attachment was forwarded from is no longer in the store",
        ));
    }
    let mut mail = assembly.mail;
    for file in assembly.files {
        mail.outgoing.attachments.push(read_file(file).await?);
    }
    for part in assembly.parts {
        mail.outgoing
            .attachments
            .push(fetch_part(provider, part).await?);
    }
    Ok(mail)
}
