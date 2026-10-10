//! Raw mode and the alternate screen. `enter` hands back a terminal whose guard restores the
//! user's terminal on every exit path, and the panic hook restores it before a panic prints.

use std::io::{self, Stdout, Write};
use std::sync::Once;

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

/// Enter raw mode and the alternate screen. The guard exists before the alternate screen
/// is entered, so every later failure still restores the terminal.
pub fn enter() -> io::Result<Tui> {
    install_panic_hook();
    enable_raw_mode()?;
    let guard = TerminalGuard::new(io::stdout());
    execute!(io::stdout(), EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(Tui {
        terminal,
        _guard: guard,
    })
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

/// Chain a hook that restores the terminal before the existing hook (color-eyre's) prints
/// the panic. Installed once per process.
pub fn install_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore(&mut io::stdout());
            previous(info);
        }));
    });
}

/// Behind the hidden `--panic-test` flag: enter the alternate screen and panic, so a binary
/// test can check that the restore sequence reaches the terminal before the panic message.
/// Raw mode is skipped because that test runs without a terminal.
pub fn panic_test() -> ! {
    install_panic_hook();
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

    #[test]
    fn guard_writes_nothing_until_dropped() {
        let buf = SharedBuf::default();
        let guard = TerminalGuard::new(buf.clone());
        assert!(written(&buf).is_empty());
        drop(guard);
        assert!(!written(&buf).is_empty());
    }
}
