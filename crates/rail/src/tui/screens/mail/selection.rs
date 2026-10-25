//! The list's selection: thread ids with what the list last showed of each (account, starred,
//! unread). Ids, not rows, so it survives re-reads and scope changes; `Esc` or an applied
//! action clears it.

use std::collections::BTreeMap;

use rail_core::store::view::ThreadRow;

#[cfg(test)]
use super::bulk::MAX_SELECTION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picked {
    pub account_id: i64,
    pub starred: bool,
    pub unread: bool,
}

impl Picked {
    pub fn of(row: &ThreadRow) -> Self {
        Self {
            account_id: row.account_id,
            starred: row.starred,
            unread: row.unread_count > 0,
        }
    }
}

#[derive(Debug, Default)]
pub struct Selection {
    picked: BTreeMap<i64, Picked>,
}

impl Selection {
    pub fn toggle(&mut self, row: &ThreadRow) {
        let id = row.key.thread_id;
        if self.picked.remove(&id).is_none() {
            self.picked.insert(id, Picked::of(row));
        }
    }

    /// Add `targets` in the given order until the selection holds `MAX_SELECTION`; later ones
    /// are dropped. True when anything was left out. The cap is on the whole selection, which
    /// survives a scope change, so repeated select-alls can never take it over `MAX_SELECTION`.
    #[cfg(test)]
    pub fn add(&mut self, targets: &[(i64, Picked)]) -> bool {
        for (id, picked) in targets {
            // An id already selected is overwritten rather than counted again.
            if self.picked.len() >= MAX_SELECTION && !self.picked.contains_key(id) {
                return true;
            }
            self.picked.insert(*id, *picked);
        }
        false
    }

    /// Put back targets an action took when it failed.
    pub fn restore(&mut self, targets: &[(i64, Picked)]) {
        self.picked.extend(targets.iter().copied());
    }

    pub fn clear(&mut self) {
        self.picked.clear();
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.picked.is_empty()
    }

    pub fn contains(&self, thread: i64) -> bool {
        self.picked.contains_key(&thread)
    }

    /// Selected rows a read returned update their flags.
    pub fn refresh(&mut self, rows: &[ThreadRow]) {
        for row in rows {
            if let Some(picked) = self.picked.get_mut(&row.key.thread_id) {
                *picked = Picked::of(row);
            }
        }
    }

    pub fn targets(&self) -> Vec<(i64, Picked)> {
        self.picked.iter().map(|(id, p)| (*id, *p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_core::store::view::{ThreadKey, ThreadRow};

    fn row(id: i64, account: i64, unread: u32, starred: bool) -> ThreadRow {
        ThreadRow {
            key: ThreadKey {
                last_date: id,
                thread_id: id,
            },
            account_id: account,
            subject: String::new(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: unread,
            starred,
            has_attachments: false,
        }
    }

    #[test]
    fn a_selection_is_ids_that_survive_rereads_and_follow_their_flags() {
        let mut s = Selection::default();
        s.toggle(&row(1, 1, 1, false));
        s.toggle(&row(2, 2, 0, true));
        assert_eq!(s.len(), 2);
        s.toggle(&row(1, 1, 1, false));
        assert!(!s.contains(1) && s.contains(2));
        s.refresh(&[row(2, 2, 3, false), row(9, 1, 0, false)]);
        assert_eq!(
            s.targets(),
            [(
                2,
                Picked {
                    account_id: 2,
                    starred: false,
                    unread: true
                }
            )]
        );
        s.clear();
        assert!(s.is_empty());
    }

    fn picked(account: i64) -> Picked {
        Picked {
            account_id: account,
            starred: false,
            unread: true,
        }
    }

    /// `n` targets of account 1, ids `from..from + n`.
    fn targets(from: i64, n: usize) -> Vec<(i64, Picked)> {
        (0..i64::try_from(n).unwrap())
            .map(|i| (from + i, picked(1)))
            .collect()
    }

    #[test]
    fn add_fills_up_to_the_cap_in_order_and_says_what_it_dropped() {
        let mut s = Selection::default();
        assert!(!s.add(&targets(1, 3)), "nothing dropped under the cap");
        assert_eq!(s.len(), 3);

        // An id already selected is overwritten, not counted again.
        assert!(!s.add(&[(2, picked(7))]));
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.targets().iter().find(|(id, _)| *id == 2).unwrap().1,
            picked(7),
            "the newer flags win"
        );

        // The cap is on the whole selection, so repeated adds can never exceed it.
        let mut s = Selection::default();
        assert!(!s.add(&targets(1, MAX_SELECTION - 1)));
        assert!(
            s.add(&targets(i64::try_from(MAX_SELECTION).unwrap(), 5)),
            "the add that crosses the cap reports the drop"
        );
        assert_eq!(s.len(), MAX_SELECTION);
        assert!(
            s.contains(i64::try_from(MAX_SELECTION).unwrap()),
            "the first of the slice is kept"
        );
        assert!(
            !s.contains(i64::try_from(MAX_SELECTION).unwrap() + 1),
            "the ones after it are dropped, in the slice's order"
        );
        assert!(
            s.add(&targets(900_000, 1)),
            "a full selection takes nothing"
        );
        assert_eq!(s.len(), MAX_SELECTION);

        // `restore` puts a failed call's own targets back and is not capped.
        s.restore(&targets(900_000, 3));
        assert_eq!(s.len(), MAX_SELECTION + 3);
    }
}
