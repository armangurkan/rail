//! Raw mode and the alternate screen. `enter` hands back a terminal whose guard restores the
//! user's terminal on every exit path, and the panic hook restores it before a panic prints.

use std::io::{self, Stdout, Write};
use std::panic::PanicHookInfo;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, ThreadId};

use crossterm::cursor::Show;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// The terminal the shell draws on, plus the guard that puts the user's terminal back.
pub struct Tui {
    /// Declared first so it drops before the guard restores the terminal.
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    _guard: TerminalGuard<Stdout>,
}

impl Drop for Tui {
    fn drop(&mut self) {
        hook_state().set_tui_thread(None);
    }
}

/// Enter raw mode and the alternate screen. The guard exists before the alternate screen
/// is entered, so every later failure still restores the terminal.
pub fn enter() -> io::Result<Tui> {
    install_panic_hook();
    activate_after(hook_state(), || {
        enable_raw_mode()?;
        let guard = TerminalGuard::new(io::stdout());
        execute!(io::stdout(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok(Tui {
            terminal,
            _guard: guard,
        })
    })
}

/// Run `setup`, and only if it succeeds record this thread as the one that owns the terminal.
/// A failed setup leaves no `Tui` to clear the record, so it must not be written first.
fn activate_after<T>(state: &HookState, setup: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let value = setup()?;
    state.set_tui_thread(Some(thread::current().id()));
    Ok(value)
}

/// Restores the terminal when dropped: raw mode off, alternate screen left, cursor shown.
pub struct TerminalGuard<W: Write> {
    out: W,
}

impl<W: Write> TerminalGuard<W> {
    /// Guard `out`; nothing is written until the guard is dropped.
    pub fn new(out: W) -> Self {
        Self { out }
    }
}

impl<W: Write> Drop for TerminalGuard<W> {
    fn drop(&mut self) {
        restore(&mut self.out);
    }
}

/// Best effort by design: this runs while dropping or panicking, where an error has nowhere
/// to go, and each step is attempted even if an earlier one failed. Disabling raw mode is a
/// no-op when this process never enabled it.
fn restore(out: &mut impl Write) {
    let _ = disable_raw_mode();
    let _ = execute!(out, LeaveAlternateScreen, Show);
}

/// What the panic hook reads: which thread owns the terminal, and how to restore it.
struct HookState {
    tui_thread: Mutex<Option<ThreadId>>,
    restore: Box<dyn Fn() + Send + Sync>,
}

impl HookState {
    fn set_tui_thread(&self, id: Option<ThreadId>) {
        if let Ok(mut slot) = self.tui_thread.lock() {
            *slot = id;
        }
    }

    /// Where a panic on this thread happens relative to the TUI.
    fn site(&self) -> PanicSite {
        match self.tui_thread.lock().map(|slot| *slot).unwrap_or(None) {
            None => PanicSite::NoTui,
            Some(id) if id == thread::current().id() => PanicSite::TuiThread,
            Some(_) => PanicSite::OtherThread,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PanicSite {
    /// The thread that entered the TUI: restore the terminal, then print.
    TuiThread,
    /// No TUI is active: the terminal is not ours, so just print.
    NoTui,
    /// A background job while the TUI is up: leave the terminal alone and log.
    OtherThread,
}

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync>;

/// Restore the terminal and defer to `previous` (which prints) for a panic on the TUI thread;
/// with no TUI active, only defer. A panic on any other thread while the TUI is up is a
/// background job: leave the terminal alone and log one line, because the task supervisor
/// reports it to the screen.
fn panic_hook(state: Arc<HookState>, previous: Hook) -> Hook {
    Box::new(move |info| match state.site() {
        PanicSite::TuiThread => {
            (state.restore)();
            previous(info);
        }
        PanicSite::NoTui => previous(info),
        PanicSite::OtherThread => {
            let location = info.location().map(ToString::to_string).unwrap_or_default();
            tracing::error!(location, "background task panicked: {}", payload_text(info));
        }
    })
}

/// The panic message when the payload is a string; never the Debug of arbitrary data.
fn payload_text(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

fn hook_state() -> &'static Arc<HookState> {
    static STATE: OnceLock<Arc<HookState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Arc::new(HookState {
            tui_thread: Mutex::new(None),
            restore: Box::new(|| restore(&mut io::stdout())),
        })
    })
}

/// Chain a hook that restores the terminal before the existing hook (color-eyre's) prints
/// the panic, for panics on the TUI thread only. Installed once per process.
pub fn install_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(panic_hook(hook_state().clone(), previous));
    });
}

/// Behind the hidden `--panic-test` flag: enter the alternate screen and panic, so a binary
/// test can check that the restore sequence reaches the terminal before the panic message.
/// Raw mode is skipped because that test runs without a terminal.
pub fn panic_test() -> ! {
    install_panic_hook();
    hook_state().set_tui_thread(Some(thread::current().id()));
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    panic!("rail panic test: panicking inside the alternate screen");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("buffer lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn written(buf: &SharedBuf) -> String {
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn guard_leaves_the_alternate_screen_and_shows_the_cursor_on_drop() {
        let buf = SharedBuf::default();
        drop(TerminalGuard::new(buf.clone()));
        let out = written(&buf);
        assert!(out.contains("\x1b[?1049l"), "{out:?}");
        assert!(out.contains("\x1b[?25h"), "{out:?}");
    }

    /// Install a hook over test-owned state, panic once, and return (restored, printed). The
    /// hook is process-global, so this swaps it out and back under a lock.
    fn hook_flags(tui: Option<ThreadId>, panic_on_worker: bool) -> (bool, bool) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static HOOK_TESTS: Mutex<()> = Mutex::new(());
        let _serial = HOOK_TESTS.lock().unwrap();

        let restored = Arc::new(AtomicBool::new(false));
        let printed = Arc::new(AtomicBool::new(false));
        let state = Arc::new(HookState {
            tui_thread: Mutex::new(tui),
            restore: {
                let restored = restored.clone();
                Box::new(move || restored.store(true, Ordering::SeqCst))
            },
        });
        let previous = {
            let printed = printed.clone();
            Box::new(move |_: &PanicHookInfo<'_>| printed.store(true, Ordering::SeqCst))
        };
        let original = std::panic::take_hook();
        std::panic::set_hook(panic_hook(state, previous));
        let outcome = if panic_on_worker {
            thread::spawn(|| panic!("job boom")).join().map(|_| ())
        } else {
            std::panic::catch_unwind(|| panic!("tui boom"))
        };
        std::panic::set_hook(original);
        assert!(outcome.is_err());
        (
            restored.load(Ordering::SeqCst),
            printed.load(Ordering::SeqCst),
        )
    }

    #[test]
    fn hook_leaves_the_terminal_alone_for_panics_off_the_tui_thread() {
        let flags = hook_flags(Some(thread::current().id()), true);
        assert_eq!(flags, (false, false), "a job panic touched the terminal");
    }

    #[test]
    fn hook_restores_then_prints_for_panics_on_the_tui_thread() {
        let flags = hook_flags(Some(thread::current().id()), false);
        assert_eq!(flags, (true, true));
    }

    #[test]
    fn hook_only_prints_when_no_tui_is_active() {
        assert_eq!(
            hook_flags(None, false),
            (false, true),
            "wrote restore bytes"
        );
        assert_eq!(hook_flags(None, true), (false, true), "wrote restore bytes");
    }

    fn fresh_state() -> HookState {
        HookState {
            tui_thread: Mutex::new(None),
            restore: Box::new(|| {}),
        }
    }

    #[test]
    fn a_failed_enter_leaves_the_tui_thread_unset() {
        let state = fresh_state();
        let result = activate_after(&state, || -> io::Result<()> {
            Err(io::Error::other("no tty"))
        });
        assert!(result.is_err());
        assert_eq!(state.site(), PanicSite::NoTui);
    }

    #[test]
    fn a_successful_enter_records_the_tui_thread() {
        let state = fresh_state();
        activate_after(&state, || Ok(())).unwrap();
        assert_eq!(state.site(), PanicSite::TuiThread);
    }

    #[test]
    fn guard_writes_nothing_until_dropped() {
        let buf = SharedBuf::default();
        let guard = TerminalGuard::new(buf.clone());
        assert!(written(&buf).is_empty());
        drop(guard);
        assert!(!written(&buf).is_empty());
    }
}
