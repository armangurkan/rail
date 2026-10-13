//! What an action does, independent of any provider or store: the scope rule a one-folder
//! mailbox's adapter and the store's optimistic change both follow.

pub mod scope;
#[cfg(test)]
mod scope_tests;
