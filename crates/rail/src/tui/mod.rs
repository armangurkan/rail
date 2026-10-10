//! The terminal shell: raw mode and the alternate screen, the event loop, and the screens
//! that run inside it. Nothing here prints text to stdout or stderr while the alternate
//! screen is up.
// Parts of the shell are only reached from tests until `rail setup` runs it.
#![allow(dead_code)]

pub mod app;
pub mod run;
pub mod screen;
pub mod terminal;
#[cfg(test)]
pub mod test_util;
pub mod theme;
pub mod widgets;
