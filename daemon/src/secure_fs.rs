//! Ownership and mode checks for the daemon's home folder and secret files.
//!
//! The daemon keeps a pairing token, a plugin copy that embeds the token, and
//! the job inbox/outbox under one folder. A folder or file that another
//! account created first (or a symlink that points elsewhere) must never be
//! trusted. At startup every one of these paths is checked with `lstat`: the
//! daemon refuses a symlink and refuses an owner that is not the effective
//! user. The error names the path and the reason, so the user can fix it.

use std::io;
use std::path::{Path, PathBuf};

/// The payload of every refusal this module returns. `is_refusal` finds it,
/// so a caller can tell a refusal (a setup problem the user must fix) from an
/// ordinary I/O error.
#[derive(Debug)]
struct Refusal(String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// Builds the refusal error. The message names the path and the reason.
fn refuse(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        Refusal(format!("refusing to use {}: {reason}", path.display())),
    )
}

/// True when `err` is a refusal from this module.
pub fn is_refusal(err: &io::Error) -> bool {
    err.get_ref().is_some_and(|inner| inner.is::<Refusal>())
}

/// The process exit code for a failed startup. A refusal under the launchd
/// service exits 0: the plist restarts only on a non-zero exit, and a
/// restart cannot fix a folder the user must repair. Every other failure
/// exits 1.
pub fn startup_exit_code(refusal: bool, supervised: bool) -> i32 {
    if refusal && supervised {
        0
    } else {
        1
    }
}

/// Exit code for a startup error `err` in the current environment.
pub fn exit_code_for_error(err: &io::Error) -> i32 {
    startup_exit_code(is_refusal(err), crate::supervisor::is_supervised())
}

const SYMLINK_FIX: &str =
    "it is a symlink; replace the link with the real folder, or set TURBOFIG_BRIDGE_DIR=<target>";

/// The pure half of the ownership check. `is_symlink` and `owner_uid` come
/// from `lstat`; `euid` is the effective user of this process.
pub(crate) fn check_identity(
    path: &Path,
    is_symlink: bool,
    owner_uid: u32,
    euid: u32,
) -> io::Result<()> {
    if is_symlink {
        return Err(refuse(path, SYMLINK_FIX));
    }
    if owner_uid != euid {
        return Err(refuse(
            path,
            &format!(
                "it is owned by uid {owner_uid}, not by this user (uid {euid}); delete it or chown it to your user"
            ),
        ));
    }
    Ok(())
}

/// Runs `lstat` on `path` and applies `check_identity`. A missing path is
/// fine (the caller creates it). Returns the metadata of an existing path.
#[cfg(unix)]
pub(crate) fn check_existing(path: &Path) -> io::Result<Option<std::fs::Metadata>> {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    check_identity(path, meta.file_type().is_symlink(), meta.uid(), euid)?;
    Ok(Some(meta))
}

#[cfg(not(unix))]
pub(crate) fn check_existing(path: &Path) -> io::Result<Option<std::fs::Metadata>> {
    Ok(std::fs::symlink_metadata(path).ok())
}

/// The pure half of the ancestor check. An ancestor of the bridge folder must
/// belong to this user or to root. It must not be writable by group or other,
/// unless it has the sticky bit (like `/tmp`): in a writable folder without
/// the sticky bit, another account can rename the bridge folder and swap in
/// its own `inbox`.
pub(crate) fn check_ancestor(path: &Path, owner_uid: u32, mode: u32, euid: u32) -> io::Result<()> {
    if owner_uid != euid && owner_uid != 0 {
        return Err(refuse(
            path,
            &format!(
                "it is a parent of the bridge folder and is owned by uid {owner_uid}; \
                 set TURBOFIG_BRIDGE_DIR to a folder inside a folder you own"
            ),
        ));
    }
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Err(refuse(
            path,
            &format!(
                "it is a parent of the bridge folder and other accounts can write to it (mode {:03o}); \
                 run chmod go-w on it, or set TURBOFIG_BRIDGE_DIR to a folder inside a folder you own",
                mode & 0o777
            ),
        ));
    }
    Ok(())
}

/// Checks every ancestor of `dir`, from the nearest existing one up to `/`.
/// The parent is canonicalized (not the leaf), so the leaf symlink check
/// still applies to `dir` itself. A missing parent is skipped: the walk
/// starts at the first ancestor that exists.
#[cfg(unix)]
pub(crate) fn check_ancestors(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let abs = std::path::absolute(dir)?;
    let Some(parent) = abs.parent() else {
        return Ok(());
    };
    let mut start = parent;
    let canon = loop {
        match std::fs::canonicalize(start) {
            Ok(c) => break c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => match start.parent() {
                Some(p) => start = p,
                None => return Ok(()),
            },
            Err(e) => return Err(e),
        }
    };
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    for ancestor in canon.ancestors() {
        let meta = std::fs::metadata(ancestor)?;
        check_ancestor(ancestor, meta.uid(), meta.permissions().mode(), euid)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn check_ancestors(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Name of the marker file the daemon writes into an outbox it owns.
pub(crate) const OUTBOX_MARKER: &str = ".turbofig-outbox";

/// Writes the outbox marker. The prune runs only in an outbox with a marker.
#[cfg(unix)]
fn write_outbox_marker(outbox: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(outbox.join(OUTBOX_MARKER))
        .map(|_| ())
}

#[cfg(not(unix))]
fn write_outbox_marker(outbox: &Path) -> io::Result<()> {
    std::fs::File::create(outbox.join(OUTBOX_MARKER)).map(|_| ())
}

/// Creates `path` (and parents) if missing, then refuses a symlink or a
/// foreign owner. When `set_mode` is true, sets the mode to 0700 and treats a
/// failure as fatal. A non-directory at `path` is refused too.
pub(crate) fn ensure_private_dir(path: &Path, set_mode: bool) -> io::Result<()> {
    if check_existing(path)?.is_none() {
        std::fs::create_dir_all(path)?;
    }
    let meta = check_existing(path)?.ok_or_else(|| refuse(path, "it vanished during startup"))?;
    if !meta.is_dir() {
        return Err(refuse(path, "it is not a directory"));
    }
    if set_mode {
        set_owner_only(path).map_err(|e| refuse(path, &format!("could not set mode 0700: {e}")))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Checks the bridge folder and creates `inbox/` and `outbox/` in it.
///
/// `own_mode` is true for the default home the daemon owns outright: its mode
/// is set to 0700. A custom `TURBOFIG_BRIDGE_DIR` keeps its own mode, but it
/// still gets the symlink and owner checks. `inbox/` and `outbox/` are always
/// set to 0700. A failed chmod is fatal.
pub(crate) fn prepare_bridge_dirs(dir: &Path, own_mode: bool) -> io::Result<(PathBuf, PathBuf)> {
    let inbox = dir.join("inbox");
    let outbox = dir.join("outbox");
    check_ancestors(dir)?;
    ensure_private_dir(dir, own_mode)?;
    ensure_private_dir(&inbox, true)?;
    let outbox_is_new = check_existing(&outbox)?.is_none();
    ensure_private_dir(&outbox, true)?;
    // The marker proves the daemon made this outbox. The default home also
    // gets it, because older versions created that outbox without a marker.
    if outbox_is_new || own_mode {
        write_outbox_marker(&outbox)
            .map_err(|e| refuse(&outbox, &format!("could not write {OUTBOX_MARKER}: {e}")))?;
    }
    Ok((inbox, outbox))
}

/// Name of the single-instance lock file in the bridge folder.
pub(crate) const DAEMON_LOCK: &str = "daemon.lock";

/// The one lock fd of this process. The daemon never closes it: a POSIX
/// `fcntl` lock is dropped when ANY fd to the file closes in the process, so
/// nothing else in the process may open `daemon.lock`.
#[cfg(unix)]
static HELD_LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

#[cfg(unix)]
fn flock_struct(l_type: libc::c_short) -> libc::flock {
    // SAFETY: `flock` is plain data; all zero is a valid value.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = l_type;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    fl
}

/// `fcntl` errors that mean the filesystem has no byte-range locks.
#[cfg(unix)]
fn lock_errno_means_unsupported(errno: i32) -> bool {
    matches!(
        errno,
        libc::ENOLCK | libc::ENOTSUP | libc::EOPNOTSUPP | libc::ENOSYS
    )
}

/// Result of `lock_bridge_dir`.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
pub enum LockOutcome {
    /// This process now holds the lock for its whole life.
    Held,
    /// Another process holds it.
    Busy,
    /// The folder's filesystem has no `fcntl` locks (SMB, NFS, FUSE): the
    /// daemon runs without a lock rather than crash-looping under launchd.
    Unsupported,
}

/// Takes the daemon's exclusive lock on `<dir>/daemon.lock`. Creates the
/// bridge folder if missing and applies the same checks as the other bridge
/// files (no symlink, owned by this user, safe ancestors). The lock file is
/// opened with `O_NOFOLLOW`, mode 0600. Retries `F_SETLK` for `retry` so a
/// racing check or a daemon that is just exiting does not cause a false
/// refusal.
#[cfg(unix)]
pub fn lock_bridge_dir(
    dir: &Path,
    own_mode: bool,
    retry: std::time::Duration,
) -> io::Result<LockOutcome> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::os::unix::io::AsRawFd;
    // A second call must not open the file again: closing any fd to it would
    // drop the lock the first call took.
    if HELD_LOCK.get().is_some() {
        return Err(io::Error::other(
            "this process already holds a bridge folder lock",
        ));
    }
    check_ancestors(dir)?;
    ensure_private_dir(dir, own_mode)?;
    let path = dir.join(DAEMON_LOCK);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let meta = file.metadata()?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if !meta.is_file() {
        return Err(refuse(&path, "it is not a regular file"));
    }
    if meta.uid() != euid {
        return Err(refuse(&path, "it is owned by another user"));
    }
    let until = std::time::Instant::now() + retry;
    loop {
        let fl = flock_struct(libc::F_WRLCK as libc::c_short);
        // SAFETY: valid fd and a valid `flock` struct.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &fl) };
        if rc == 0 {
            // Keep the only fd open for the life of the process.
            return match HELD_LOCK.set(file) {
                Ok(()) => Ok(LockOutcome::Held),
                Err(file) => {
                    // Closing it would release the lock: leak it instead.
                    std::mem::forget(file);
                    Err(io::Error::other(
                        "this process already holds a bridge folder lock",
                    ))
                }
            };
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EACCES) => {}
            Some(e) if lock_errno_means_unsupported(e) => return Ok(LockOutcome::Unsupported),
            _ => return Err(err),
        }
        if std::time::Instant::now() >= until {
            return Ok(LockOutcome::Busy);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Writes `{"pid":..,"requestTimeoutMs":..}` into the held lock file, in
/// place, through the held fd (no second fd is ever opened: that would drop
/// the lock). `turbofig js` reads it to size its wait. Best effort.
#[cfg(unix)]
pub fn write_lock_info(request_timeout_ms: u64) {
    use std::os::unix::fs::FileExt;
    let Some(file) = HELD_LOCK.get() else {
        return;
    };
    let body = format!(
        "{{\"pid\":{},\"requestTimeoutMs\":{request_timeout_ms}}}\n",
        std::process::id()
    );
    if file.set_len(0).is_ok() {
        let _ = file.write_all_at(body.as_bytes(), 0);
    }
}

/// The request timeout the daemon recorded in its lock file, if readable.
/// Opens the file read-only with `O_NOFOLLOW`, and requires a regular file
/// owned by this user.
#[cfg(unix)]
pub(crate) fn read_lock_request_timeout(dir: &Path) -> Option<std::time::Duration> {
    use std::io::Read;
    let mut file = open_lock_readonly(dir)?;
    let mut buf = String::new();
    (&mut file).take(4096).read_to_string(&mut buf).ok()?;
    let v: serde_json::Value = serde_json::from_str(buf.trim()).ok()?;
    let ms = v.get("requestTimeoutMs")?.as_u64()?;
    (ms > 0).then(|| std::time::Duration::from_millis(ms))
}

/// Opens `<dir>/daemon.lock` read-only (never creating it) and checks the
/// open handle: a regular file owned by this user.
#[cfg(unix)]
fn open_lock_readonly(dir: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(DAEMON_LOCK))
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    (meta.is_file() && meta.uid() == euid).then_some(file)
}

/// Refuses a folder that group or other can write to, unless it has the
/// sticky bit: another account could then swap `inbox/` or `outbox/`.
#[cfg(unix)]
pub(crate) fn check_not_shared_writable(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let Some(meta) = check_existing(dir)? else {
        return Ok(());
    };
    let mode = meta.permissions().mode();
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Err(refuse(
            dir,
            &format!(
                "other accounts can write to it (mode {:03o}); run chmod go-w on it",
                mode & 0o777
            ),
        ));
    }
    Ok(())
}

/// True when some process holds the daemon lock in `dir`. Queries with
/// `F_GETLK`, which never takes the lock, so it cannot race the daemon's
/// `F_SETLK`. Opens the file read-only with `O_NOFOLLOW` and never creates
/// it. A read lock is the query type: it conflicts with the daemon's write
/// lock and needs no write access on the fd. No file means no holder.
#[cfg(unix)]
pub(crate) fn bridge_lock_held(dir: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Some(file) = open_lock_readonly(dir) else {
        return false;
    };
    let mut fl = flock_struct(libc::F_RDLCK as libc::c_short);
    // SAFETY: valid fd and a valid `flock` struct.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut fl) };
    rc == 0 && fl.l_type != libc::F_UNLCK as libc::c_short
}

/// Opens `path` with `O_NOFOLLOW`, checks the open handle (regular file,
/// owner, mode with no 0o077 bit), and reads from that same handle. Checking
/// and reading one handle leaves no gap in which the file could be swapped.
#[cfg(unix)]
pub(crate) fn read_token_nofollow(path: &Path) -> io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                refuse(path, SYMLINK_FIX)
            } else {
                e
            }
        })?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(refuse(path, "it is not a regular file"));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    check_identity(path, false, meta.uid(), euid)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(refuse(
            path,
            &format!("its mode is {mode:03o}; chmod 600 it or delete it"),
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

#[cfg(not(unix))]
pub(crate) fn read_token_nofollow(path: &Path) -> io::Result<String> {
    std::fs::read_to_string(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn check_identity_refuses_a_foreign_owner() {
        let err = check_identity(Path::new("/x"), false, 501, 502).unwrap_err();
        assert!(err.to_string().contains("owned by uid 501"), "{err}");
        assert!(err.to_string().contains("/x"), "{err}");
        assert!(check_identity(Path::new("/x"), false, 502, 502).is_ok());
    }

    #[test]
    fn check_identity_refuses_a_symlink() {
        let err = check_identity(Path::new("/x"), true, 502, 502).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    #[test]
    fn fresh_dir_is_accepted_and_set_to_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("fresh");
        let (inbox, outbox) = prepare_bridge_dirs(&dir, true).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&inbox), 0o700);
        assert_eq!(mode(&outbox), 0o700);
        // A normal restart also passes.
        prepare_bridge_dirs(&dir, true).unwrap();
    }

    #[test]
    fn symlinked_bridge_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        let err = prepare_bridge_dirs(&link, true).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        let err = prepare_bridge_dirs(&link, false).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    #[test]
    fn symlinked_inbox_and_outbox_are_refused() {
        for name in ["inbox", "outbox"] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("home");
            std::fs::create_dir(&dir).unwrap();
            let target = tmp.path().join("elsewhere");
            std::fs::create_dir(&target).unwrap();
            symlink(&target, dir.join(name)).unwrap();
            let err = prepare_bridge_dirs(&dir, true).unwrap_err();
            assert!(err.to_string().contains("symlink"), "{name}: {err}");
        }
    }

    #[test]
    fn a_file_in_place_of_a_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("home");
        std::fs::write(&dir, b"x").unwrap();
        let err = prepare_bridge_dirs(&dir, true).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn token_file_with_group_or_world_bits_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("token");
        std::fs::write(&path, "a".repeat(64)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_token_nofollow(&path).unwrap_err();
        assert!(
            err.to_string().contains("chmod 600 it or delete it"),
            "{err}"
        );
        assert!(is_refusal(&err));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_nofollow(&path).unwrap().len(), 64);
    }

    #[test]
    fn symlinked_token_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::write(&real, "a".repeat(64)).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = tmp.path().join("token");
        symlink(&real, &link).unwrap();
        let err = read_token_nofollow(&link).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(is_refusal(&err));
    }

    #[test]
    fn check_ancestor_refuses_a_foreign_owner() {
        let err = check_ancestor(Path::new("/x"), 501, 0o755, 502).unwrap_err();
        assert!(err.to_string().contains("owned by uid 501"), "{err}");
        assert!(err.to_string().contains("/x"), "{err}");
        assert!(check_ancestor(Path::new("/x"), 502, 0o755, 502).is_ok());
        assert!(check_ancestor(Path::new("/x"), 0, 0o755, 502).is_ok());
    }

    #[test]
    fn check_ancestor_refuses_other_writable_unless_sticky() {
        for mode in [0o777, 0o775, 0o757] {
            let err = check_ancestor(Path::new("/x"), 502, mode, 502).unwrap_err();
            assert!(err.to_string().contains("can write"), "{mode:o}: {err}");
        }
        assert!(check_ancestor(Path::new("/tmp"), 0, 0o1777, 502).is_ok());
        assert!(check_ancestor(Path::new("/x"), 502, 0o700, 502).is_ok());
    }

    #[test]
    fn default_home_ancestors_pass() {
        let home = std::env::var("HOME").expect("HOME");
        check_ancestors(&Path::new(&home).join(".turbofig")).unwrap();
    }

    #[test]
    fn an_other_writable_parent_blocks_the_bridge_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = prepare_bridge_dirs(&open.join("bridge"), false).unwrap_err();
        assert!(
            err.to_string().contains("other accounts can write"),
            "{err}"
        );
        assert!(is_refusal(&err));
        assert!(!open.join("bridge").exists());
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o1777)).unwrap();
        prepare_bridge_dirs(&open.join("bridge"), false).unwrap();
    }

    #[test]
    fn marker_is_written_for_a_new_outbox_and_the_default_home_only() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = tmp.path().join("fresh");
        prepare_bridge_dirs(&fresh, false).unwrap();
        assert!(fresh.join("outbox").join(OUTBOX_MARKER).is_file());

        let custom = tmp.path().join("custom");
        std::fs::create_dir_all(custom.join("outbox")).unwrap();
        prepare_bridge_dirs(&custom, false).unwrap();
        assert!(!custom.join("outbox").join(OUTBOX_MARKER).exists());

        let legacy = tmp.path().join("legacy");
        std::fs::create_dir_all(legacy.join("outbox")).unwrap();
        prepare_bridge_dirs(&legacy, true).unwrap();
        assert!(legacy.join("outbox").join(OUTBOX_MARKER).is_file());
    }

    #[test]
    fn symlinked_parent_of_the_bridge_dir_is_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        symlink(&real, &link).unwrap();
        prepare_bridge_dirs(&link.join("home"), true).unwrap();
        assert!(real.join("home/inbox").is_dir());
    }

    #[test]
    fn refusals_are_recognised_and_other_errors_are_not() {
        assert!(is_refusal(&refuse(Path::new("/x"), "why")));
        assert!(!is_refusal(&io::Error::other("boom")));
        assert!(!is_refusal(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn startup_exit_code_is_0_only_for_a_supervised_refusal() {
        assert_eq!(startup_exit_code(true, true), 0);
        assert_eq!(startup_exit_code(true, false), 1);
        assert_eq!(startup_exit_code(false, true), 1);
        assert_eq!(startup_exit_code(false, false), 1);
    }

    #[test]
    fn missing_token_file_is_an_ordinary_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = read_token_nofollow(&tmp.path().join("token")).unwrap_err();
        assert!(!is_refusal(&err));
    }

    #[test]
    fn lock_file_symlink_is_refused_and_a_lock_is_never_created_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("b");
        std::fs::create_dir(&dir).unwrap();
        let target = tmp.path().join("target");
        std::os::unix::fs::symlink(&target, dir.join(DAEMON_LOCK)).unwrap();
        let r = lock_bridge_dir(&dir, false, std::time::Duration::ZERO);
        assert!(r.is_err());
        assert!(!target.exists());
        assert!(!bridge_lock_held(&dir));
    }

    #[test]
    fn no_lock_file_means_no_holder_and_the_query_does_not_create_it() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!bridge_lock_held(tmp.path()));
        assert!(!tmp.path().join(DAEMON_LOCK).exists());
    }

    #[test]
    fn lock_errors_for_filesystems_without_locks_are_recognised() {
        for e in [libc::ENOLCK, libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS] {
            assert!(lock_errno_means_unsupported(e));
        }
        for e in [libc::EAGAIN, libc::EACCES, libc::EBADF, libc::EIO] {
            assert!(!lock_errno_means_unsupported(e));
        }
    }

    #[test]
    fn a_folder_other_accounts_can_write_to_is_refused_unless_sticky() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("b");
        std::fs::create_dir(&d).unwrap();
        for (mode, ok) in [
            (0o700, true),
            (0o755, true),
            (0o775, false),
            (0o777, false),
            (0o1777, true),
        ] {
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(check_not_shared_writable(&d).is_ok(), ok, "{mode:o}");
        }
    }

    #[test]
    fn lock_info_is_read_back_and_a_symlinked_or_missing_file_gives_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_lock_request_timeout(tmp.path()), None);
        let f = tmp.path().join(DAEMON_LOCK);
        std::fs::write(&f, "{\"pid\":1,\"requestTimeoutMs\":45000}\n").unwrap();
        assert_eq!(
            read_lock_request_timeout(tmp.path()),
            Some(std::time::Duration::from_millis(45000))
        );
        std::fs::write(&f, "garbage").unwrap();
        assert_eq!(read_lock_request_timeout(tmp.path()), None);
        std::fs::remove_file(&f).unwrap();
        let real = tmp.path().join("real");
        std::fs::write(&real, "{\"requestTimeoutMs\":1000}").unwrap();
        std::os::unix::fs::symlink(&real, &f).unwrap();
        assert_eq!(read_lock_request_timeout(tmp.path()), None);
    }
}
