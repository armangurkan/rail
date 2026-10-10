use std::process::{Child, Command, Stdio};

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
/// the terminal. The opener is not waited on here (`xdg-open` and `open` hand off and exit);
/// a thread reaps it, so a long-lived daemon that opens many logins leaves no zombies.
pub fn launch(url: &Url) -> std::io::Result<()> {
    let child = Command::new(opener())
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    reap(child);
    Ok(())
}

/// Wait for `child` on a thread of its own, so its exit status is collected whenever it ends.
pub fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_reaped_child_leaves_no_zombie() {
        let child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        reap(child);
        let stat = format!("/proc/{pid}/stat");
        for _ in 0..200 {
            // Gone entirely once reaped; a zombie would linger with state `Z`.
            if !std::path::Path::new(&stat).exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let state = std::fs::read_to_string(&stat).unwrap_or_default();
        panic!("child {pid} was never reaped: {state}");
    }
}
