//! The list's window: at most `MAX_WINDOW` rows around the selection, extended a page at a time
//! by keyset as the selection nears an edge, and replaced on a jump or a re-read.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use rail_core::store::view::{PAGE, PageAt, ThreadPage, ThreadRow};

use crate::tui::clock::{Clock, Group};

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
    /// Bumped by every read that changes the rows: what the date groups are worked out from.
    generation: u64,
}

impl ListWindow {
    pub fn rows(&self) -> &[ThreadRow] {
        &self.rows
    }

    /// Bumped by every read the window takes.
    pub fn generation(&self) -> u64 {
        self.generation
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

    /// A page down (`forward`) or up from a screen whose first thread is `top` and that showed
    /// `threads` threads. `last_of(s)` is the last thread a screen starting at thread `s`
    /// draws. The next screen starts with the thread after the last one shown; the screen
    /// above is the one that ends with the thread before the first one shown (or the first
    /// that reaches it, when group rows make an exact fit impossible). The cursor keeps its
    /// row on the new screen, or goes to the new screen's last thread when it holds fewer
    /// threads, so no thread is passed over unseen. Says which page to read, as `move_by` does.
    pub fn page(
        &mut self,
        forward: bool,
        top: usize,
        threads: usize,
        last_of: impl Fn(usize) -> usize,
    ) -> Option<PageAt> {
        if self.rows.is_empty() {
            return None;
        }
        let n = threads.max(1);
        let last = self.rows.len() - 1;
        let row = self.selected.saturating_sub(top);
        let start = match (forward, top) {
            (true, _) => (top + n).min(last),
            (false, 0) => {
                self.selected = 0;
                self.offset = 0;
                return self.wanted(n);
            }
            (false, _) => screen_above(top, &last_of),
        };
        self.selected = (start + row).min(last_of(start)).min(last);
        self.offset = start.min(self.selected);
        self.show(n);
        self.wanted(n)
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
    /// The first row on screen is the newer of `offset` and `drawn`, the first thread the last
    /// frame drew: a frame held at the last full screen draws threads above `offset`.
    pub fn requery_at(&self, drawn: Option<usize>) -> PageAt {
        let first = drawn.map_or(self.offset, |drawn| drawn.min(self.offset));
        match self.rows.get(first) {
            Some(top) if first > 0 || self.newer => PageAt::From(top.key),
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
        self.generation = self.generation.wrapping_add(1);
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

/// The first thread of the screen above the one starting at thread `top` (> 0): the earliest
/// start whose screen still reaches thread `top - 1`, scanning up from it while the screen one
/// thread higher reaches it too.
fn screen_above(top: usize, last_of: &impl Fn(usize) -> usize) -> usize {
    let before = top - 1;
    let mut start = before;
    while start > 0 && last_of(start - 1) >= before {
        start -= 1;
    }
    start
}

/// The window's generation and the day the groups were worked out for, and the groups.
type Cached = (u64, Option<jiff::civil::Date>, Rc<[Group]>);

/// The window's date groups, worked out once per read of the window and per day, not on every
/// frame.
#[derive(Debug, Default)]
pub struct GroupCache {
    cached: RefCell<Option<Cached>>,
    /// How many times they were worked out.
    builds: Cell<usize>,
}

impl GroupCache {
    /// Each row's group, in the window's order.
    pub fn groups(&self, list: &ListWindow, clock: &Clock) -> Rc<[Group]> {
        let (generation, day) = (list.generation(), clock.today());
        if let Some((g, d, groups)) = self.cached.borrow().as_ref()
            && *g == generation
            && *d == day
        {
            return groups.clone();
        }
        let groups: Rc<[Group]> = list
            .rows()
            .iter()
            .map(|r| clock.group(r.key.last_date))
            .collect();
        self.builds.set(self.builds.get() + 1);
        *self.cached.borrow_mut() = Some((generation, day, groups.clone()));
        groups
    }

    /// How many times the groups were worked out (tests).
    #[cfg(test)]
    pub fn builds(&self) -> usize {
        self.builds.get()
    }
}

#[cfg(test)]
mod tests {
    use rail_core::store::view::{ThreadKey, ThreadPage, ThreadRow};

    use super::*;

    /// Thread `i`, newest first by index.
    fn row(i: usize) -> ThreadRow {
        let n = i64::try_from(i).unwrap();
        ThreadRow {
            key: ThreadKey {
                last_date: 1_760_000_000 - n,
                thread_id: n,
            },
            account_id: 1,
            subject: format!("t{i}"),
            participants: Vec::new(),
            message_count: 1,
            unread_count: 0,
            starred: false,
            has_attachments: false,
        }
    }

    /// The last thread of a screen of twenty threads starting at `s`.
    fn twenty(s: usize) -> usize {
        s + 19
    }

    /// A window of `n` threads read from the top, `older` when more lie beyond.
    fn window(n: usize, older: bool) -> ListWindow {
        let mut list = ListWindow::default();
        let page = ThreadPage {
            rows: (0..n).map(row).collect(),
            older,
            newer: false,
        };
        list.apply(page, PageAt::First, Place::Top, 20);
        list
    }

    #[test]
    fn every_read_bumps_the_generation() {
        let mut list = window(3, false);
        let first = list.generation();
        let page = ThreadPage {
            rows: (0..3).map(row).collect(),
            older: false,
            newer: false,
        };
        list.apply(page, PageAt::First, Place::Keep, 20);
        assert_eq!(list.generation(), first + 1);
    }

    /// Review Focus 2.
    #[test]
    fn a_page_down_near_the_windows_end_asks_for_the_older_page() {
        let mut list = window(30, true);
        let last = list.rows()[29].key;
        assert_eq!(list.page(true, 0, 20, twenty), Some(PageAt::Older(last)));
        assert_eq!((list.selected_index(), list.offset()), (20, 20));
        assert_eq!(list.page(true, 20, 20, twenty), None, "a read is in flight");
        assert_eq!(
            list.selected_index(),
            29,
            "the selection still moves, to the last row"
        );
    }

    #[test]
    fn a_page_up_from_the_top_stays_at_the_top_and_reads_nothing() {
        let mut list = window(30, false);
        assert_eq!(list.page(false, 0, 20, twenty), None);
        assert_eq!((list.selected_index(), list.offset()), (0, 0));
    }
}
