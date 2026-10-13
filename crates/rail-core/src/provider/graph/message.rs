//! Graph messages as Rail envelopes. Every string is outside text, put on one line and cut as
//! Gmail's are; ids Rail would not send are refused. Graph has no `In-Reply-To`, `References`
//! or size: they stay empty in Part A.

use super::dates::parse_ms;
use super::graph_id;
use super::wire::{Flag, MessageWire, Recipient};
use crate::provider::gmail::metadata::one_line_capped;
use crate::provider::gmail::{MAX_ADDRESSES, MAX_SNIPPET_CHARS, MAX_SUBJECT_CHARS};
use crate::provider::{Envelope, Mailbox};

const MAX_NAME_CHARS: usize = 200;
const MAX_ADDRESS_CHARS: usize = 254;
const MAX_ID_CHARS: usize = 998;
/// A sent time this far from the receive time is not believed, as Gmail's `Date:` is not.
const MAX_DATE_SKEW_MS: i64 = 366 * 86_400_000;
const FLAGGED: &str = "flagged";

/// The envelope of `wire`, in `folder` (`None`: a folder Rail does not sync, so none). `None`
/// when Rail cannot read it: no safe id or conversation id, or no usable receive time.
pub(crate) fn envelope(wire: MessageWire, folder: Option<&str>) -> Option<Envelope> {
    let remote_id = wire.id.filter(|id| graph_id(id))?;
    let thread_id = wire.conversation_id.filter(|id| graph_id(id))?;
    let internal_date = parse_ms(wire.received.as_deref()?)?;
    let sent = wire
        .sent
        .as_deref()
        .and_then(parse_ms)
        .filter(|sent| sent.abs_diff(internal_date) <= MAX_DATE_SKEW_MS.unsigned_abs());
    let from = wire
        .from
        .as_ref()
        .and_then(mailbox)
        .or_else(|| wire.sender.as_ref().and_then(mailbox));
    let message_id = wire
        .internet_message_id
        .as_deref()
        .map(header_id)
        .filter(|id| !id.is_empty());
    Some(Envelope {
        remote_id,
        thread_id,
        message_id,
        in_reply_to: None,
        references: Vec::new(),
        subject: one_line_capped(
            wire.subject.as_deref().unwrap_or_default(),
            MAX_SUBJECT_CHARS,
        ),
        from: from.into_iter().collect(),
        to: mailboxes(wire.to.as_deref()),
        cc: mailboxes(wire.cc.as_deref()),
        reply_to: mailboxes(wire.reply_to.as_deref()),
        bcc: mailboxes(wire.bcc.as_deref()),
        date: sent.unwrap_or(internal_date).div_euclid(1000),
        internal_date,
        snippet: one_line_capped(
            wire.body_preview.as_deref().unwrap_or_default(),
            MAX_SNIPPET_CHARS,
        ),
        folders: folder.map(|f| vec![f.to_string()]).unwrap_or_default(),
        is_read: wire.is_read.unwrap_or(false),
        is_starred: wire.flag.as_ref().is_some_and(flagged),
        is_draft: wire.is_draft.unwrap_or(false),
        size: 0,
    })
}

/// Outlook's flag as Rail's star.
pub(crate) fn flagged(flag: &Flag) -> bool {
    flag.flag_status
        .as_deref()
        .is_some_and(|status| status.eq_ignore_ascii_case(FLAGGED))
}

/// `internetMessageId` as the store keeps a `Message-ID`: without its angle brackets.
fn header_id(raw: &str) -> String {
    let trimmed = raw.trim();
    let bare = trimmed
        .strip_prefix('<')
        .and_then(|id| id.strip_suffix('>'))
        .unwrap_or(trimmed);
    one_line_capped(bare, MAX_ID_CHARS)
}

fn mailbox(recipient: &Recipient) -> Option<Mailbox> {
    let email = recipient.email_address.as_ref()?;
    let address = email
        .address
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())?;
    Some(Mailbox {
        name: email
            .name
            .as_deref()
            .map(|name| one_line_capped(name, MAX_NAME_CHARS))
            .filter(|name| !name.is_empty()),
        address: one_line_capped(address, MAX_ADDRESS_CHARS),
    })
}

fn mailboxes(list: Option<&[Recipient]>) -> Vec<Mailbox> {
    list.unwrap_or_default()
        .iter()
        .filter_map(mailbox)
        .take(MAX_ADDRESSES)
        .collect()
}
