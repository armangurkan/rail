//! The list's window: at most `MAX_WINDOW` rows around the selection, extended a page at a time
//! by keyset as the selection nears an edge, and replaced on a jump or a re-read.

use std::collections::HashSet;

use rail_core::store::view::{PAGE, PageAt, ThreadPage, ThreadRow};

/// Rows the window holds at most.
pub const MAX_WINDOW: usize = 3 * PAGE;

/// Where the selection goes when a read replaces the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// On the thread selected before (by id), or at its index when it is gone.
    Keep,
    Top,
    Bottom,
}

#[derive(Debug, Default)]
pub struct ListWindow {
    rows: Vec<ThreadRow>,
    selected: usize,
    /// The first row on screen.
    offset: usize,
    /// More threads lie beyond the window's last row / first row.
    older: bool,
    newer: bool,
    /// A page read is in flight.
    loading: bool,
}

impl ListWindow {
    pub fn rows(&self) -> &[ThreadRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&ThreadRow> {
        self.rows.get(self.selected)
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Move the selection by `delta` rows (clamped), keep it on a screen of `height` rows,
    /// and say which page to read when it came within a screen of an edge with more beyond.
    pub fn move_by(&mut self, delta: isize, height: usize) -> Option<PageAt> {
        if self.rows.is_empty() {
            return None;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len() - 1);
        self.show(height);
        self.wanted(height)
    }

    /// The newest thread: a read of the first page when the window does not hold it.
    pub fn top(&mut self, height: usize) -> Option<PageAt> {
        if self.newer {
            self.loading = true;
            return Some(PageAt::First);
        }
        self.selected = 0;
        self.show(height);
        None
    }

    /// The oldest thread: a read of the last page when the window does not hold it.
    pub fn bottom(&mut self, height: usize) -> Option<PageAt> {
        if self.older {
            self.loading = true;
            return Some(PageAt::Last);
        }
        self.selected = self.rows.len().saturating_sub(1);
        self.show(height);
        None
    }

    fn wanted(&mut self, height: usize) -> Option<PageAt> {
        if self.loading {
            return None;
        }
        let near_bottom = self.selected + height >= self.rows.len();
        let near_top = self.selected < height;
        let at = if near_bottom && self.older {
            self.rows.last().map(|r| PageAt::Older(r.key))
        } else if near_top && self.newer {
            self.rows.first().map(|r| PageAt::Newer(r.key))
        } else {
            None
        };
        self.loading = at.is_some();
        at
    }

    fn show(&mut self, height: usize) {
        let height = height.max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
    }

    /// Where a re-read after a store change starts: from the first row on screen, so the
    /// selection (always on screen, and a screen is shorter than a page) is in the page that
    /// comes back; from the top when the screen starts at the newest thread (so new mail shows).
    pub fn requery_at(&self) -> PageAt {
        match self.rows.get(self.offset) {
            Some(top) if self.offset > 0 || self.newer => PageAt::From(top.key),
            _ => PageAt::First,
        }
    }

    /// A page read in flight is stale now; its answer will be dropped.
    pub fn cancel_loading(&mut self) {
        self.loading = false;
    }

    /// The answer to a read at `at`: `Older` and `Newer` extend the window, the others
    /// replace it and put the selection at `place`.
    pub fn apply(&mut self, page: ThreadPage, at: PageAt, place: Place, height: usize) {
        self.loading = false;
        let keep = self.selected().map(|r| r.key.thread_id);
        let top = self.rows.get(self.offset).map(|r| r.key.thread_id);
        let index = self.selected;
        match at {
            PageAt::Older(_) => self.append(page),
            PageAt::Newer(_) => self.prepend(page),
            PageAt::First | PageAt::Last | PageAt::From(_) => {
                self.rows = page.rows;
                self.older = page.older;
                self.newer = page.newer;
            }
        }
        let last = self.rows.len().saturating_sub(1);
        self.selected = match place {
            Place::Top => 0,
            Place::Bottom => last,
            Place::Keep => self.find(keep).unwrap_or(index).min(last),
        };
        self.offset = self.find(top).unwrap_or(self.offset).min(last);
        self.show(height);
    }

    fn find(&self, thread: Option<i64>) -> Option<usize> {
        let thread = thread?;
        self.rows.iter().position(|r| r.key.thread_id == thread)
    }

    /// Older rows below; a stale copy of any of them leaves the window first.
    fn append(&mut self, page: ThreadPage) {
        let fresh: HashSet<i64> = page.rows.iter().map(|r| r.key.thread_id).collect();
        self.rows.retain(|r| !fresh.contains(&r.key.thread_id));
        self.rows.extend(page.rows);
        self.older = page.older;
        let over = self.rows.len().saturating_sub(MAX_WINDOW);
        if over > 0 {
            self.rows.drain(..over);
            self.newer = true;
        }
    }

    /// Newer rows above; a stale copy of any of them leaves the window first.
    fn prepend(&mut self, page: ThreadPage) {
        let fresh: HashSet<i64> = page.rows.iter().map(|r| r.key.thread_id).collect();
        self.rows.retain(|r| !fresh.contains(&r.key.thread_id));
        let mut rows = page.rows;
        rows.append(&mut self.rows);
        self.rows = rows;
        self.newer = page.newer;
        if self.rows.len() > MAX_WINDOW {
            self.rows.truncate(MAX_WINDOW);
            self.older = true;
        }
    }
}
