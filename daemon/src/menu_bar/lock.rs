//! Single-instance guard: an exclusive, non-blocking `flock` on
//! `<home>/app.lock`. A second menu-bar launch while one is already running
//! must never create a second tray icon; `try_acquire` returning `None` is
//! that second launch's signal to exit at once (step 3 makes it bring up the
//! setup window in the first instance instead; for now it only exits).
//!
//! `flock` state lives on the open file description, not the directory
//! entry or its contents, so this never needs to read, write, or delete the
//! lock file's contents: an empty, possibly-stale file still locks cleanly,
//! and the lock releases the instant the holding process exits or drops
//! `AppLock`, with no cleanup step required.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Holds the exclusive lock for as long as this value lives. Dropping it
/// (including on process exit) closes the file descriptor, which releases
/// the `flock` automatically.
pub struct AppLock {
    _file: File,
}

/// Attempts to exclusively lock `path`, creating it first if needed.
/// `Ok(Some(_))`: this process now holds the lock. `Ok(None)`: another
/// process already holds it. `Err`: any other failure (e.g. the directory
/// does not exist, or a permissions problem), which the caller should treat
/// as fatal rather than silently proceeding as a second instance.
pub fn try_acquire(path: &Path) -> io::Result<Option<AppLock>> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(AppLock { _file: file }));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_attempt_while_the_first_is_held_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("app.lock");

        let first = try_acquire(&lock_path).expect("first attempt");
        assert!(first.is_some(), "the first attempt must acquire the lock");

        let second = try_acquire(&lock_path).expect("second attempt");
        assert!(
            second.is_none(),
            "a second attempt while the first is held must fail"
        );
    }

    #[test]
    fn a_second_attempt_succeeds_once_the_first_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("app.lock");

        let first = try_acquire(&lock_path).expect("first attempt");
        assert!(first.is_some());
        drop(first);

        let second = try_acquire(&lock_path).expect("second attempt");
        assert!(
            second.is_some(),
            "a second attempt after the first is dropped must succeed"
        );
    }

    #[test]
    fn it_creates_the_lock_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("app.lock");
        assert!(!lock_path.exists());

        let _lock = try_acquire(&lock_path).expect("acquire");
        assert!(lock_path.exists());
    }
}
