//! The two saved-search calls the mail screen sends, and the scripted answers the screen
//! tests get. A sibling of `daemon_link.rs` rather than part of it: that file is already close
//! to the 800-line cap, and one op's method plus its fake handler is ~40 lines.

use rail_core::ops::op::{SavedSearchPut, SavedSearchRemove};
use rail_core::ops::{SavedSearchListResponse, SavedSearchPutRequest, SavedSearchRemoveRequest};

#[cfg(test)]
use super::daemon_link::FakeCall;
use super::daemon_link::Link;
use super::{DaemonLink, MailEvent, job, send};
use crate::tui::app::Effect;

impl DaemonLink {
    /// Save `name` for `query`, or rename the search `replaces` names. A rename sends no
    /// query, so the daemon keeps the one in the file (#109). The answer is the whole new
    /// list, so the screen never merges one itself.
    pub fn saved_search_put(
        &self,
        name: String,
        query: Option<String>,
        replaces: Option<String>,
    ) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                // Captured here, where the rename is sent, so its answer carries it: every
                // answer is the whole list, so nothing in the answer itself says which write
                // it belongs to.
                let renamed = replaces.clone().map(|old| (old, name.clone()));
                let request = SavedSearchPutRequest {
                    name,
                    query,
                    replaces,
                };
                let result = connector.call::<SavedSearchPut>(&request).await;
                send(&sink, changed(result, renamed));
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move {
                fake.saved_search_put(&sink, name, query, replaces.as_deref());
            }),
        }
    }

    /// Remove the search named `name`. The mail it finds is not touched.
    pub fn saved_search_remove(&self, name: String) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = SavedSearchRemoveRequest { name };
                let result = connector.call::<SavedSearchRemove>(&request).await;
                send(&sink, changed(result, None));
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move {
                fake.saved_search_remove(&sink, &name);
            }),
        }
    }
}

/// Either op's answer as the screen's event: the new list, or why nothing changed. `renamed`
/// is the pair the request asked for, so the screen follows a rename only on its own answer.
fn changed(
    result: Result<SavedSearchListResponse, rail_core::ops::OpError>,
    renamed: Option<(String, String)>,
) -> MailEvent {
    MailEvent::SavedSearchesChanged {
        result: result.map(|answer| answer.searches),
        renamed,
    }
}

#[cfg(test)]
impl super::FakeDaemon {
    /// Record the call and answer: `saved_answer` when a test set one, else the request
    /// applied to the fake's own list — `replaces` edits that entry in place (keeping its
    /// position, and its query when none is sent, as the daemon's own edit does), any other
    /// name is appended.
    pub(super) fn saved_search_put(
        &self,
        sink: &crate::tui::app::TaskSink,
        name: String,
        query: Option<String>,
        replaces: Option<&str>,
    ) {
        self.record(FakeCall::SavedSearchPut {
            name: name.clone(),
            query: query.clone(),
            replaces: replaces.map(str::to_string),
        });
        // As the real link does: taken from the request, before `name` is moved into the entry.
        let renamed = replaces.map(|old| (old.to_string(), name.clone()));
        let answer = self.saved_answer.lock().unwrap().take();
        let result = answer.unwrap_or_else(|| {
            let mut saved = self.saved.lock().unwrap();
            let at = replaces
                .and_then(|old| saved.iter().position(|s| s.name.eq_ignore_ascii_case(old)));
            let kept = at.map(|at| saved[at].query.clone());
            let query = query.or(kept).unwrap_or_default();
            let entry = rail_core::config::SavedSearch { name, query };
            match at {
                Some(at) => saved[at] = entry,
                None => saved.push(entry),
            }
            Ok(SavedSearchListResponse {
                searches: saved.clone(),
            })
        });
        send(sink, changed(result, renamed));
    }

    /// Record the call and answer: `saved_answer` when a test set one, else the fake's own
    /// list with `name` deleted.
    pub(super) fn saved_search_remove(&self, sink: &crate::tui::app::TaskSink, name: &str) {
        self.record(FakeCall::SavedSearchRemove {
            name: name.to_string(),
        });
        let answer = self.saved_answer.lock().unwrap().take();
        let result = answer.unwrap_or_else(|| {
            let mut saved = self.saved.lock().unwrap();
            saved.retain(|s| !s.name.eq_ignore_ascii_case(name));
            Ok(SavedSearchListResponse {
                searches: saved.clone(),
            })
        });
        send(sink, changed(result, None));
    }
}
