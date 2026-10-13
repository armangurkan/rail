//! From a draft row to an `OutgoingMail`: addresses, headers, what the provider needs besides
//! the message (kind, source, its copy of the draft, thread), and what still has to be read
//! (files on disk, parts of stored messages) before the message can go.

use crate::provider::{
    Carried, DraftIds, Mailbox, Outgoing, OutgoingAttachment, OutgoingMail, Provider, ProviderError,
};
use crate::store::drafts::{Attachment, Draft, DraftKind, ListedFile, Recipient};
use crate::store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileRef {
    pub path: String,
    pub name: String,
    pub mime: String,
    /// The size the provider listed for its copy, when recorded.
    pub listed: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PartRef {
    pub message_remote: String,
    pub part: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
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
                path,
                name,
                mime,
                listed,
                ..
            } => files.push(FileRef {
                path: path.clone(),
                name: name.clone(),
                mime: mime.clone(),
                listed: *listed,
            }),
            Attachment::Part {
                message,
                part,
                name,
                mime,
                size,
            } => match store.message_remote_id(account_id, *message)? {
                Some(remote) => parts.push(PartRef {
                    message_remote: remote,
                    part: part.clone(),
                    name: name.clone(),
                    mime: mime.clone(),
                    size: *size,
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
        listed: file.listed.and_then(|l| usize::try_from(l).ok()),
    })
}

/// One part of a stored message, fetched from the provider (`provider_name` in the error).
async fn fetch_part(
    provider: &dyn Provider,
    part: PartRef,
    provider_name: &str,
) -> Result<OutgoingAttachment, ProviderError> {
    let bytes = provider
        .attachment(&part.message_remote, &part.part)
        .await
        .map_err(|e| match e {
            ProviderError::NotFound => ProviderError::permanent(&format!(
                "an attachment of this message is no longer in {provider_name}"
            )),
            other => other,
        })?;
    Ok(OutgoingAttachment {
        name: part.name,
        mime: part.mime,
        bytes,
        carried: None,
        listed: None,
    })
}

/// A forwarded original the provider's own forward carries: its name and size only; the
/// provider fetches its bytes only if its copy is missing.
fn carried(part: PartRef) -> OutgoingAttachment {
    OutgoingAttachment {
        name: part.name,
        mime: part.mime,
        bytes: Vec::new(),
        carried: Some(Carried {
            message: part.message_remote,
            part: part.part,
            size: usize::try_from(part.size).unwrap_or(0),
        }),
        listed: None,
    }
}

/// Read the files and fetch the parts; a forward's parts on a provider whose forward carries
/// them (Graph) are passed as `Carried`, unread. A file or part that cannot be had is a
/// permanent failure, its error naming `provider_name`, never the attachment.
pub(super) async fn attach(
    assembly: Assembly,
    provider: &dyn Provider,
    provider_name: &str,
) -> Result<OutgoingMail, ProviderError> {
    if assembly.part_gone {
        return Err(ProviderError::permanent(
            "the message an attachment was forwarded from is no longer in the store",
        ));
    }
    let carry = provider.forward_carries_originals() && assembly.mail.kind == DraftKind::Forward;
    let mut mail = assembly.mail;
    for file in assembly.files {
        mail.outgoing.attachments.push(read_file(file).await?);
    }
    for part in assembly.parts {
        let attachment = if carry {
            carried(part)
        } else {
            fetch_part(provider, part, provider_name).await?
        };
        mail.outgoing.attachments.push(attachment);
    }
    Ok(mail)
}

/// The paths of the assembly's files, in the order `attach` puts them first in the message.
pub(super) fn file_paths(assembly: &Assembly) -> Vec<String> {
    assembly.files.iter().map(|f| f.path.clone()).collect()
}

/// The sizes a save says the provider lists for the message's files (the first `paths.len()`
/// attachments, each read from the file at that path), for the draft to record.
pub(super) fn listed_files(
    paths: &[String],
    mail: &OutgoingMail,
    listed: &[Option<usize>],
) -> Vec<ListedFile> {
    paths
        .iter()
        .zip(&mail.outgoing.attachments)
        .zip(listed)
        .filter_map(|((path, attachment), listed)| {
            Some(ListedFile {
                path: path.clone(),
                size: i64::try_from(attachment.bytes.len()).ok()?,
                listed: i64::try_from((*listed)?).ok()?,
            })
        })
        .collect()
}

/// The message with the sizes a save says the provider now lists for its attachments, so the
/// next save of it (Graph's send saves again) matches them.
pub(super) fn with_listed(mail: &mut OutgoingMail, listed: &[Option<usize>]) {
    for (attachment, listed) in mail.outgoing.attachments.iter_mut().zip(listed) {
        if listed.is_some() {
            attachment.listed = *listed;
        }
    }
}
