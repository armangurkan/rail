//! `format=full` messages as Rail bodies. HTML is preferred and rendered to text by html2text,
//! which drops scripts and styles and fetches nothing; plain text is decoded from its charset.
//! The text keeps its lines and tabs and loses every other control and format character. A
//! body over `MAX_BODY_BYTES` is cut and ends with `TRUNCATED_MARKER`.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use super::html_depth::{MAX_HTML_DEPTH, limit_depth};
use super::metadata::cut;
use super::wire::{AttachmentBody, Message, Part};
use super::{COST_GET, GmailProvider, checked_id, safe_id};
use crate::config::model::is_format_char;
use crate::provider::{AttachmentMeta, Body, ProviderError};

/// The most body text (and HTML) Rail keeps for one message.
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// The end of a body Rail cut at `MAX_BODY_BYTES`.
pub const TRUNCATED_MARKER: &str = "\n\n[Rail kept only the first 10 MiB of this message.]";
const TEXT_WIDTH: usize = 100;
const MAX_PART_DEPTH: usize = 32;
const MAX_ATTACHMENTS: usize = 100;
const MAX_FILENAME_CHARS: usize = 255;
const MAX_MIME_CHARS: usize = 255;
const MAX_ATTACHMENT_ID: usize = 1024;
/// A part id is dot-separated numbers (`0`, `1.2`); each one at most this long.
const MAX_PART_SEGMENT: usize = 16;
const MAX_PART_ID: usize = 64;
const MAX_CHARSET_CHARS: usize = 40;

const URL_SAFE: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const STANDARD: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Html,
    Plain,
}

/// The text part a body is made from.
pub(crate) struct Chosen {
    pub kind: Kind,
    pub charset: Option<String>,
    pub data: Option<String>,
    pub attachment_id: Option<String>,
}

#[derive(Default)]
pub(crate) struct Walked {
    pub html: Option<Chosen>,
    pub plain: Option<Chosen>,
    pub attachments: Vec<AttachmentMeta>,
}

fn unreadable() -> ProviderError {
    ProviderError::permanent("Rail could not read this message's body")
}

impl GmailProvider {
    /// Message `id`'s body. A body Rail cannot read is a permanent error, logged by id only, so
    /// the caller skips that one message and goes on.
    pub(crate) async fn full_body(&self, id: &str) -> Result<Body, ProviderError> {
        let result = self.read_body(id).await;
        if let Err(error @ ProviderError::Permanent { .. }) = &result {
            tracing::warn!(
                id = %checked_id(id).unwrap_or("<unsafe id>"),
                reason = %error,
                "skipped a message body Rail could not read"
            );
        }
        result
    }

    async fn read_body(&self, id: &str) -> Result<Body, ProviderError> {
        let id = checked_id(id).ok_or_else(unreadable)?;
        let query = [("format", "full".to_string())];
        let message: Message = self
            .api
            .get_json(&format!("messages/{id}"), &query, COST_GET)
            .await?;
        if message.id.as_deref() != Some(id) {
            return Err(unreadable());
        }
        let walked = walk(&message.payload.ok_or_else(unreadable)?);
        let Some(chosen) = walked.html.or(walked.plain) else {
            return Ok(Body {
                attachments: walked.attachments,
                ..Body::default()
            });
        };
        let data = match (chosen.data, chosen.attachment_id) {
            (Some(data), _) => data,
            (None, Some(attachment)) => self.attachment_data(id, &attachment).await?,
            (None, None) => String::new(),
        };
        let (kind, charset, attachments) = (chosen.kind, chosen.charset, walked.attachments);
        tokio::task::spawn_blocking(move || {
            let bytes = decode_base64(&data)?;
            render(kind, &bytes, charset.as_deref(), attachments)
        })
        .await
        .map_err(|_| unreadable())?
    }

    /// A body part Gmail sent separately (`messages.attachments.get`, read only).
    async fn attachment_data(&self, id: &str, attachment: &str) -> Result<String, ProviderError> {
        if !safe_id(attachment, MAX_ATTACHMENT_ID) {
            return Err(unreadable());
        }
        let path = format!("messages/{id}/attachments/{attachment}");
        let body: AttachmentBody = self.api.get_json(&path, &[], COST_GET).await?;
        Ok(body.data.unwrap_or_default())
    }
}

/// The first HTML and plain-text parts that are not attachments, and every attachment.
pub(crate) fn walk(payload: &Part) -> Walked {
    let mut walked = Walked::default();
    visit(payload, &mut walked, 0);
    walked
}

fn visit(part: &Part, walked: &mut Walked, depth: usize) {
    if depth > MAX_PART_DEPTH {
        return;
    }
    let mime = part.mime_type.to_ascii_lowercase();
    if !part.filename.is_empty() {
        if walked.attachments.len() < MAX_ATTACHMENTS && part_id_ok(&part.part_id) {
            walked.attachments.push(AttachmentMeta {
                part_id: part.part_id.clone(),
                filename: part.filename.chars().take(MAX_FILENAME_CHARS).collect(),
                mime_type: mime.chars().take(MAX_MIME_CHARS).collect(),
                size: part.body.size.max(0),
            });
        }
        return;
    }
    let chosen = |kind| Chosen {
        kind,
        charset: charset(part),
        data: part.body.data.clone(),
        attachment_id: part.body.attachment_id.clone(),
    };
    match mime.as_str() {
        "text/html" if walked.html.is_none() => walked.html = Some(chosen(Kind::Html)),
        "text/plain" if walked.plain.is_none() => walked.plain = Some(chosen(Kind::Plain)),
        _ => {}
    }
    for child in &part.parts {
        visit(child, walked, depth + 1);
    }
}

/// A part id Rail keeps: empty (the message itself) or dot-separated safe segments.
fn part_id_ok(id: &str) -> bool {
    id.is_empty()
        || (id.len() <= MAX_PART_ID && id.split('.').all(|s| safe_id(s, MAX_PART_SEGMENT)))
}

/// The `charset` parameter of the part's `Content-Type`.
fn charset(part: &Part) -> Option<String> {
    let header = part
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-type"))?;
    let lower = header.value.to_ascii_lowercase();
    let start = lower.find("charset=")? + "charset=".len();
    let name: String = header.value[start..]
        .trim_start_matches('"')
        .chars()
        .take_while(|c| !matches!(c, '"' | ';' | ' ' | '\t'))
        .take(MAX_CHARSET_CHARS)
        .collect();
    (!name.is_empty()).then_some(name)
}

fn decode_base64(data: &str) -> Result<Vec<u8>, ProviderError> {
    URL_SAFE
        .decode(data.as_bytes())
        .or_else(|_| STANDARD.decode(data.as_bytes()))
        .map_err(|_| unreadable())
}

/// Decode `data` from `charset`, cut to `MAX_BODY_BYTES`, render HTML to text, and clean the
/// text of control and format characters.
pub(crate) fn render(
    kind: Kind,
    data: &[u8],
    charset: Option<&str>,
    attachments: Vec<AttachmentMeta>,
) -> Result<Body, ProviderError> {
    let decoded = to_text(&data[..data.len().min(MAX_BODY_BYTES)], charset);
    let kept = cut(&decoded, MAX_BODY_BYTES);
    let rendered = match kind {
        Kind::Html => clean(&html_to_text(kept)?),
        Kind::Plain => clean(kept),
    };
    let mut text = cut(&rendered, MAX_BODY_BYTES).to_string();
    let truncated =
        data.len() > MAX_BODY_BYTES || decoded.len() > kept.len() || rendered.len() > text.len();
    if truncated {
        text.push_str(TRUNCATED_MARKER);
    }
    Ok(Body {
        text,
        html: (kind == Kind::Html).then(|| kept.to_string()),
        truncated,
        attachments,
    })
}

fn to_text(bytes: &[u8], charset: Option<&str>) -> String {
    let label = charset.unwrap_or("utf-8").to_ascii_lowercase();
    if matches!(label.as_str(), "utf-8" | "utf8" | "us-ascii" | "ascii") {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    match mail_parser::decoders::charsets::map::charset_decoder(label.as_bytes()) {
        Some(decode) => decode(bytes),
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// html2text renders offline: it parses with html5ever and drops `head`, `script`, `style`
/// and `link`; an `img` becomes its alt text. Nothing is fetched. Nesting is bounded first
/// (html5ever is quadratic in depth), and deep indentation may run past `TEXT_WIDTH` rather
/// than fail.
fn html_to_text(html: &str) -> Result<String, ProviderError> {
    let html = limit_depth(html, MAX_HTML_DEPTH);
    html2text::config::plain_no_decorate()
        .allow_width_overflow()
        .string_from_read(html.as_bytes(), TEXT_WIDTH)
        .map(|text| text.trim_end().to_string())
        .map_err(|_| unreadable())
}

/// `text` with lines (CRLF and a lone CR become LF) and tabs kept, and every other control
/// character (ESC, NUL, C1...) and format character (bidi controls, zero-width characters,
/// the byte-order mark) removed.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' | '\u{2028}' | '\u{2029}' => out.push('\n'),
            '\n' | '\t' => out.push(c),
            c if c.is_control() || is_format_char(c) => {}
            c => out.push(c),
        }
    }
    out
}
