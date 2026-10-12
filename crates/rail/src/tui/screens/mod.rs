//! Full-screen views. The wizard now; the mail views in Phase 4.

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the mail screen (Task 7) is the first caller")
)]
pub mod compose;
pub mod mail;
pub mod reader;
pub mod wizard;
mod wizard_view;
