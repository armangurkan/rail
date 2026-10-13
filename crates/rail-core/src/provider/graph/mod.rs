//! Microsoft 365 through Microsoft Graph v1.0, over the shared `ApiClient` in its Graph dialect
//! (immutable ids on every request, Graph's error envelope, at most `GRAPH_CONCURRENCY`
//! requests in flight). Graph's own JSON never leaves this module; every string it returns is
//! outside text.

pub mod errors;
#[cfg(test)]
mod errors_tests;

/// The API root every request is relative to; `me` is the signed-in mailbox.
pub const GRAPH_API: &str = "https://graph.microsoft.com/v1.0/";
/// Requests in flight at once for one mailbox: Outlook's limit per app and mailbox.
pub const GRAPH_CONCURRENCY: u32 = 4;
