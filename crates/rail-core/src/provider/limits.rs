//! Limits every provider applies to what it reads, kept once.

/// The most attachments Rail lists for one message.
pub(crate) const MAX_ATTACHMENTS: usize = 100;
/// The longest attachment file name Rail keeps, in characters.
pub(crate) const MAX_FILENAME_CHARS: usize = 255;
/// The longest provider error text a detail keeps, in characters (Gmail's message; Graph's
/// code and its message, each).
pub(crate) const MAX_ERROR_TEXT: usize = 500;
