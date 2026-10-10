use std::process::{Command, Stdio};

use url::Url;

/// Print the URL and try to open it. Printing first means a failed opener still leaves the user a way in.
/// The opener is not waited on; the caller keeps running until the login completes.
pub fn open_in_browser(url: &Url) {
    println!("Open this URL in your browser if it does not open by itself:\n\n  {url}\n");
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = Command::new(opener)
        .arg(url.as_str())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        eprintln!("rail: could not run {opener} ({e}); open the URL above by hand");
    }
}
