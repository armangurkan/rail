//! Reading a file a request named, safely: only a regular file is opened (checked by path
//! before the open and again on the handle, in case the path was swapped between), the open
//! cannot block on a FIFO or take a terminal, and at most a fixed number of bytes is read.
//! Client secret JSON files and preset files both go through `read_capped`.

use std::io::Read;
use std::path::Path;

/// Why `read_capped` read nothing. None of these carries a byte of the file.
#[derive(Debug)]
pub(crate) enum GuardedReadError {
    /// A directory, a device, a FIFO, a socket: never read.
    NotRegular,
    /// More than the cap.
    TooLarge,
    Io(std::io::Error),
}

/// `O_NONBLOCK | O_NOCTTY` for the platforms whose values are known (checked against the
/// `libc` crate): a swapped-in FIFO then opens without waiting for a writer, and a swapped-in
/// terminal never becomes the controlling terminal. Elsewhere no flags are added; the path and
/// handle checks still apply.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "s390x",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "loongarch64"
    )
))]
const OPEN_FLAGS: i32 = 0o4000 | 0o400;
#[cfg(target_os = "macos")]
const OPEN_FLAGS: i32 = 0x4 | 0x2_0000;
#[cfg(not(any(
    all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "arm",
            target_arch = "aarch64",
            target_arch = "riscv32",
            target_arch = "riscv64",
            target_arch = "s390x",
            target_arch = "powerpc",
            target_arch = "powerpc64",
            target_arch = "loongarch64"
        )
    ),
    target_os = "macos"
)))]
const OPEN_FLAGS: i32 = 0;

/// Open `path` for reading without side effects on the device it might name. The caller has
/// already refused anything that is not a regular file.
fn open_guarded(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, OPEN_FLAGS);
    options.open(path)
}

/// The whole of the regular file at `path`, at most `cap` bytes.
///
/// A race remains between the path check and the open: a co-user who can write to a
/// directory on `path` could swap a symlink in between, and the handle check then only proves
/// that what was opened is a regular file, not that it is the file that was checked. Rail runs
/// as the user and these paths come from that user's own clients, so only a hostile process
/// of the same user could exploit it, and such a process can read the user's files anyway.
/// Closing it fully needs an `openat2(RESOLVE_NO_SYMLINKS)` or per-component `O_NOFOLLOW`
/// walk, left for later.
pub(crate) fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, GuardedReadError> {
    // By path first, so nothing that is not a regular file (a device, a FIFO) is ever opened.
    if !std::fs::metadata(path)
        .map_err(GuardedReadError::Io)?
        .is_file()
    {
        return Err(GuardedReadError::NotRegular);
    }
    let file = open_guarded(path).map_err(GuardedReadError::Io)?;
    // Again on the open handle: the path may have been swapped between the two steps.
    if !file.metadata().map_err(GuardedReadError::Io)?.is_file() {
        return Err(GuardedReadError::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(GuardedReadError::Io)?;
    if bytes.len() as u64 > cap {
        return Err(GuardedReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regular_file_is_read_up_to_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        std::fs::write(&file, b"12345").unwrap();
        assert_eq!(read_capped(&file, 5).unwrap(), b"12345");
        assert!(matches!(
            read_capped(&file, 4),
            Err(GuardedReadError::TooLarge)
        ));
        assert!(matches!(
            read_capped(tmp.path(), 5),
            Err(GuardedReadError::NotRegular)
        ));
        assert!(matches!(
            read_capped(&tmp.path().join("nope"), 5),
            Err(GuardedReadError::Io(_))
        ));
    }
}
