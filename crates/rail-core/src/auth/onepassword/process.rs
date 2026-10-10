//! Running one `op` process: stdin written, stdout and stderr read (each capped) on their own
//! threads, the whole call bounded by one deadline.

use std::io::{Read, Write};
use std::process::{Child, ExitStatus};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{LOCKED_MARKERS, MAX_ERROR_CHARS, MAX_OUTPUT, NO_ITEM_MARKER, POLL, SecretError};

type Drain = JoinHandle<std::io::Result<Vec<u8>>>;

/// The threads moving bytes between Rail and one child.
struct Pipes {
    writer: Option<JoinHandle<std::io::Result<()>>>,
    stdout: Option<Drain>,
    stderr: Option<Drain>,
}

impl Pipes {
    /// Start the writer (when there is input) together with both readers, so no pipe can fill
    /// and stall `op` while Rail waits on another.
    fn start(child: &mut Child, input: Option<&[u8]>) -> Self {
        let writer = child
            .stdin
            .take()
            .zip(input.map(<[u8]>::to_vec))
            .map(|(mut pipe, bytes)| {
                // Dropping the pipe at the end closes it, so `op` sees the end of its input.
                std::thread::spawn(move || pipe.write_all(&bytes))
            });
        Self {
            writer,
            stdout: child.stdout.take().map(drain),
            stderr: child.stderr.take().map(drain),
        }
    }

    /// Join the threads after the child exited. A read or write error (other than `op` having
    /// hung up first) is logged without content and reported, never swallowed.
    fn finish(self) -> Result<(Vec<u8>, Vec<u8>), SecretError> {
        if let Some(writer) = self.writer {
            match writer.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Ok(Err(e)) => return Err(io_failure("write to", &e)),
                Err(_) => return Err(panicked("write to")),
            }
        }
        Ok((joined(self.stdout)?, joined(self.stderr)?))
    }
}

fn io_failure(what: &'static str, e: &std::io::Error) -> SecretError {
    tracing::warn!(kind = ?e.kind(), what, "op pipe failed");
    SecretError::OnePasswordIo {
        what,
        message: e.to_string(),
    }
}

fn panicked(what: &'static str) -> SecretError {
    tracing::warn!(what, "a thread moving op's data panicked");
    SecretError::OnePasswordIo {
        what,
        message: "the thread panicked".to_string(),
    }
}

fn drain(pipe: impl Read + Send + 'static) -> Drain {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        pipe.take(MAX_OUTPUT)
            .read_to_end(&mut buffer)
            .map(|_| buffer)
    })
}

/// The drained bytes: output that could not be read must not pass for empty output.
fn joined(handle: Option<Drain>) -> Result<Vec<u8>, SecretError> {
    match handle.map(JoinHandle::join) {
        None => Ok(Vec::new()),
        Some(Ok(Ok(bytes))) => Ok(bytes),
        Some(Ok(Err(e))) => Err(io_failure("read from", &e)),
        Some(Err(_)) => Err(panicked("read from")),
    }
}

/// Run `child` to the end within `timeout`: stdin written, output read, process killed and
/// reaped when time runs out or waiting fails. Returns its status, stdout and stderr.
pub(super) fn run_child(
    mut child: Child,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), SecretError> {
    let pipes = Pipes::start(&mut child, input);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SecretError::OnePasswordTimeout);
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io_failure("wait for", &e));
            }
        }
    };
    let (stdout, stderr) = pipes.finish()?;
    Ok((status, stdout, stderr))
}

/// Turn `op`'s stderr into an error: its last line, without the log prefix, with every
/// redacted value removed, cut to a sane length.
pub(super) fn classify(stderr: &[u8], redact: &[&str]) -> SecretError {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("op failed without a message");
    let mut clean = strip_log_prefix(line);
    for value in redact.iter().filter(|v| !v.is_empty()) {
        clean = clean.replace(value, "<redacted>");
        // The template quotes the value as JSON, so an escaped form can appear too.
        if let Some(escaped) = json_escaped(value) {
            clean = clean.replace(&escaped, "<redacted>");
        }
    }
    let clean: String = clean.chars().take(MAX_ERROR_CHARS).collect();
    let lower = clean.to_lowercase();
    if LOCKED_MARKERS.iter().any(|m| lower.contains(m)) {
        SecretError::OnePasswordLocked(clean)
    } else if lower.contains(NO_ITEM_MARKER) {
        SecretError::OnePasswordNoItem(clean)
    } else {
        SecretError::OnePassword(clean)
    }
}

/// `value` as it appears inside a JSON string, without the quotes.
fn json_escaped(value: &str) -> Option<String> {
    let quoted = serde_json::to_string(value).ok()?;
    let inner = quoted.get(1..quoted.len().checked_sub(1)?)?;
    (inner != value).then(|| inner.to_string())
}

/// `[ERROR] 2026/10/10 08:35:58 You are not signed in` -> `You are not signed in`.
pub(super) fn strip_log_prefix(line: &str) -> String {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("[ERROR]") else {
        return line.to_string();
    };
    let mut words = rest.trim_start().splitn(3, ' ');
    match (words.next(), words.next(), words.next()) {
        (Some(date), Some(time), Some(message)) if date.contains('/') && time.contains(':') => {
            message.to_string()
        }
        _ => rest.trim().to_string(),
    }
}
