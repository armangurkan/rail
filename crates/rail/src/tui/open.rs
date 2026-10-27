//! `o`: a message's HTML for the browser, as a file under the cache directory. The file is
//! 0600 in a 0700 directory, named by a random token, and starts with a policy that blocks
//! scripts (remote images still load: the confirmation says so). Rail fetches nothing itself.
//! Files go when the reader closes, all of them when the session ends, and leftovers of a
//! crash at the next start. An opened attachment's copy goes `OPENED_GRACE` after the reader
//! closes instead, so a slow viewer still finds it.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rail_core::auth::random_token;
use rail_core::paths::{create_private_dir, write_private_atomic};

/// The directory under the cache directory.
pub const OPEN_DIR: &str = "open";
/// Files older than this at startup are a crash's leftovers. One value, shared with the
/// daemon's own sweep of the attachments it opened.
pub use rail_core::ops::STALE_AFTER;
/// What every file starts with: nothing is allowed but styles, fonts, images and media (so
/// no scripts, frames, connections, plugins, forms or base URL).
pub const CSP_PREAMBLE: &str = "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src * data:; style-src 'unsafe-inline' *; font-src *; media-src *; script-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'\">\n";

/// The files this session wrote.
#[derive(Debug)]
pub struct OpenFiles {
    dir: PathBuf,
    written: Vec<PathBuf>,
}

impl OpenFiles {
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            dir: cache_dir.join(OPEN_DIR),
            written: Vec::new(),
        }
    }

    /// Write `html` (after the policy) to a fresh private file; its path.
    pub fn write(&mut self, html: &str) -> io::Result<PathBuf> {
        create_private_dir(&self.dir)?;
        let path = self.dir.join(format!("{}.html", random_token()));
        let mut bytes = Vec::with_capacity(CSP_PREAMBLE.len() + html.len());
        bytes.extend_from_slice(CSP_PREAMBLE.as_bytes());
        bytes.extend_from_slice(html.as_bytes());
        write_private_atomic(&path, &bytes)?;
        self.written.push(path.clone());
        Ok(path)
    }

    /// Remove every file written so far. A file already gone is fine; any other failure is
    /// logged (by name only: the names are random tokens).
    pub fn remove_all(&mut self) {
        for path in self.written.drain(..) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "could not remove an opened message file"
                ),
            }
        }
    }

    /// Remove regular files in the directory last changed before `now - STALE_AFTER`; how
    /// many.
    pub fn sweep(&self, now: SystemTime) -> io::Result<usize> {
        sweep_dir(&self.dir, older_than(now, STALE_AFTER), None)
    }
}

/// How long an opened attachment's copy stays after the reader that opened it closes (#108):
/// the system opener hands off and exits at once, and a viewer that starts slowly (a cold
/// office suite, a sandboxed app) must still find the file.
pub const OPENED_GRACE: Duration = Duration::from_secs(60);
/// `OPENED_GRACE` in event-loop ticks, at one tick per `run::TICK`. A tick missed while the
/// terminal is suspended only makes the grace longer, never shorter.
pub const OPENED_GRACE_TICKS: usize =
    (OPENED_GRACE.as_millis() / crate::tui::run::TICK.as_millis()) as usize;

/// The copies `attachment.open` wrote this session. Each sits alone in a token directory
/// under the cache, so removing one never touches another's. A copy is `written` while the
/// reader that opened it is open, then `closing` until its tick deadline: removing it is a
/// check on the next tick, so nothing waits on the event loop.
#[derive(Default)]
pub struct OpenedAttachments {
    written: Vec<PathBuf>,
    /// Each closed reader's copy with the tick at which it goes.
    closing: Vec<(PathBuf, usize)>,
}

impl OpenedAttachments {
    /// Remember a path the daemon answered, so closing the reader removes it. A path already
    /// in its grace is the open reader's again and leaves the grace.
    pub fn add(&mut self, path: PathBuf) {
        self.closing.retain(|(closing, _)| *closing != path);
        self.written.push(path);
    }

    /// The reader closed at `tick`: its copies go `OPENED_GRACE_TICKS` later.
    pub fn close(&mut self, tick: usize) {
        let until = tick.saturating_add(OPENED_GRACE_TICKS);
        let closed = std::mem::take(&mut self.written);
        self.closing
            .extend(closed.into_iter().map(|path| (path, until)));
    }

    /// The event loop reached `tick`: remove the copies whose grace has run out.
    pub fn expire(&mut self, tick: usize) {
        if self.closing.iter().all(|(_, until)| *until > tick) {
            return;
        }
        let (due, kept) = std::mem::take(&mut self.closing)
            .into_iter()
            .partition(|(_, until)| *until <= tick);
        self.closing = kept;
        remove_copies(due.into_iter().map(|(path, _)| path).collect());
    }

    /// Remove every copy written so far, open or in its grace, each with the directory it was
    /// alone in: quitting does not wait.
    pub fn remove_all(&mut self) {
        let mut every = std::mem::take(&mut self.written);
        every.extend(
            std::mem::take(&mut self.closing)
                .into_iter()
                .map(|(path, _)| path),
        );
        remove_copies(every);
    }
}

/// Remove `paths`, each with the directory it was alone in. A file or directory already gone
/// is fine, and a directory still holding something is left alone. A failure is logged by
/// error kind and a count only — never by name, because unlike the random tokens `OpenFiles`
/// writes, these names are the sender's.
fn remove_copies(paths: Vec<PathBuf>) {
    let count = paths.len();
    for path in paths {
        if let Err(error) = remove_copy(&path) {
            tracing::warn!(
                kind = ?error.kind(),
                count,
                "could not remove an opened attachment"
            );
        }
    }
}

/// `path`, then the token directory it was alone in.
fn remove_copy(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    match std::fs::remove_dir(dir) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Quitting removes the copies even though the mail screen has no `Drop` of its own, as
/// `OpenFiles` does for the HTML files `o` writes.
impl Drop for OpenedAttachments {
    fn drop(&mut self) {
        self.remove_all();
    }
}

/// A count only: every path here ends in a name the sender chose.
impl std::fmt::Debug for OpenedAttachments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedAttachments")
            .field("written", &self.written.len())
            .field("closing", &self.closing.len())
            .finish()
    }
}

/// `now - age`, or the epoch when that is before it.
pub fn older_than(now: SystemTime, age: Duration) -> SystemTime {
    now.checked_sub(age).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Remove the regular files in `dir` (with `extension`, when given) last changed before
/// `cutoff`; how many. Symlinks and subdirectories are left alone (the entry's own type is
/// checked, never its target's). No directory: nothing to do.
pub fn sweep_dir(dir: &Path, cutoff: SystemTime, extension: Option<&str>) -> io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let wanted = extension.is_none_or(|x| path.extension().is_some_and(|e| e == x));
        if !wanted || !entry.file_type()?.is_file() {
            continue;
        }
        if entry.metadata()?.modified()? < cutoff {
            std::fs::remove_file(&path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

impl Drop for OpenFiles {
    fn drop(&mut self) {
        self.remove_all();
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};

    use super::*;

    #[test]
    fn files_are_private_start_with_the_policy_and_go_with_remove_all() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = OpenFiles::new(tmp.path());
        let a = files.write("<p>one</p>").unwrap();
        let b = files.write("<p>two</p>").unwrap();
        assert_ne!(a, b, "a fresh name each time");
        assert_eq!(a.parent().unwrap(), tmp.path().join(OPEN_DIR));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&a), 0o600);
        assert_eq!(mode(&tmp.path().join(OPEN_DIR)), 0o700);
        let text = std::fs::read_to_string(&a).unwrap();
        assert!(text.starts_with(CSP_PREAMBLE), "{text}");
        assert!(text.contains("default-src 'none'"));
        assert!(text.contains("script-src 'none'"));
        assert!(text.contains("img-src * data:"), "remote images still load");
        assert!(text.ends_with("<p>one</p>"));
        files.remove_all();
        assert!(!a.exists() && !b.exists());
    }

    #[test]
    fn dropping_removes_what_the_session_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let path = OpenFiles::new(tmp.path()).write("<p>x</p>").unwrap();
        assert!(!path.exists(), "the temporary OpenFiles was dropped");
        let mut files = OpenFiles::new(tmp.path());
        let kept = files.write("<p>y</p>").unwrap();
        assert!(kept.exists());
        drop(files);
        assert!(!kept.exists());
    }

    #[test]
    fn the_sweep_removes_only_old_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(OPEN_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.html");
        let fresh = dir.join("fresh.html");
        std::fs::write(&old, "x").unwrap();
        std::fs::write(&fresh, "x").unwrap();
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();
        let files = OpenFiles::new(tmp.path());
        assert_eq!(files.sweep(SystemTime::now()).unwrap(), 1);
        assert!(!old.exists() && fresh.exists());
        let none = OpenFiles::new(&tmp.path().join("nothing-here"));
        assert_eq!(
            none.sweep(SystemTime::now()).unwrap(),
            0,
            "no directory, nothing to do"
        );
    }

    /// A copy alone in a token directory under `root`, as the daemon writes one.
    fn copy_in(root: &Path, token: &str) -> PathBuf {
        let dir = root.join(token);
        create_private_dir(&dir).unwrap();
        let path = dir.join("plan.pdf");
        std::fs::write(&path, "x").unwrap();
        path
    }

    /// #108: a closed reader's copies stay `OPENED_GRACE_TICKS` ticks, then go with their
    /// directories; a copy still open is not touched.
    #[test]
    fn closed_copies_stay_for_the_grace_then_go() {
        assert_eq!(OPENED_GRACE_TICKS, 240, "a minute at one tick per 250 ms");
        let tmp = tempfile::tempdir().unwrap();
        let mut opened = OpenedAttachments::default();
        let first = copy_in(tmp.path(), "a");
        opened.add(first.clone());
        opened.close(10);
        let second = copy_in(tmp.path(), "b");
        opened.add(second.clone());
        opened.expire(10 + OPENED_GRACE_TICKS - 1);
        assert!(first.is_file(), "inside the grace");
        opened.expire(10 + OPENED_GRACE_TICKS);
        assert!(
            !first.exists() && !tmp.path().join("a").exists(),
            "after it"
        );
        assert!(second.is_file(), "a copy still open stays");
    }

    /// Dropping (quitting) removes the copies in their grace at once, and the open ones.
    #[test]
    fn dropping_removes_closed_and_open_copies_at_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opened = OpenedAttachments::default();
        let closed = copy_in(tmp.path(), "a");
        opened.add(closed.clone());
        opened.close(0);
        let open = copy_in(tmp.path(), "b");
        opened.add(open.clone());
        drop(opened);
        assert!(!closed.exists() && !open.exists());
    }

    /// A copy opened again before its grace ran out is the open reader's again, and stays.
    #[test]
    fn a_copy_opened_again_is_not_removed_by_its_old_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opened = OpenedAttachments::default();
        let path = copy_in(tmp.path(), "a");
        opened.add(path.clone());
        opened.close(0);
        opened.add(path.clone());
        opened.expire(OPENED_GRACE_TICKS * 2);
        assert!(path.is_file(), "it was opened again");
        opened.close(OPENED_GRACE_TICKS * 2);
        opened.expire(OPENED_GRACE_TICKS * 3);
        assert!(!path.exists(), "and goes a grace after the next close");
    }

    #[test]
    fn the_sweep_never_follows_a_symlink_out_of_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(OPEN_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link.html")).unwrap();
        let files = OpenFiles::new(tmp.path());
        let far_future = SystemTime::now() + Duration::from_secs(10 * 86_400);
        assert_eq!(files.sweep(far_future).unwrap(), 0);
        assert!(outside.exists());
    }
}
