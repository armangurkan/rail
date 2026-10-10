use std::process::{Command, Stdio};

use url::Url;

use crate::test_hooks;

/// The program that opens URLs: the test hook in debug builds, else the platform opener.
fn opener() -> String {
    test_hooks::opener().unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "open".to_string()
        } else {
            "xdg-open".to_string()
        }
    })
}

/// Start the browser on `url` without printing anything, so it is safe while the TUI owns
/// the terminal. The opener is not waited on; `xdg-open` and `open` hand off and exit.
pub fn launch(url: &Url) -> std::io::Result<()> {
    Command::new(opener())
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// Print the URL and try to open it. Printing first means a failed opener still leaves the
/// user a way in. Only for the flag commands; the wizard uses `launch`.
pub fn open_in_browser(url: &Url) {
    println!("Open this URL in your browser if it does not open by itself:\n\n  {url}\n");
    if let Err(e) = launch(url) {
        eprintln!(
            "rail: could not run {} ({e}); open the URL above by hand",
            opener()
        );
    }
}
