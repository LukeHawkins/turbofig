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
    check_not_shared_writable(dir)?;
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

// ------------------------------------------------------- small private files

/// Marker in the home folder: the user stopped the daemon on purpose
/// (`/control stop`, `turbofig stop`). The menu-bar app does not restart the
/// daemon while it exists. The daemon deletes it when it starts; the menu's
/// "Start turbofig" item clears it before it starts the daemon.
pub const STOPPED_BY_USER: &str = "stopped-by-user";

/// The build key of the daemon `turbofig js` last asked to restart. `js`
/// never asks the same one twice (`js_cmd::restart_older_daemon`).
pub(crate) const RESTART_ASKED: &str = "restart-asked";

/// Longest file `read_private_file` reads.
const PRIVATE_FILE_READ_CAP: u64 = 4096;

/// Writes a small file owner-only (0600) and atomically: a sibling `.tmp`
/// opened with `O_EXCL | O_NOFOLLOW` (a stale tmp or a planted symlink at that
/// name is unlinked first), then renamed over `path`.
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let written = (|| {
        let mut file = opts.open(&tmp)?;
        file.write_all(contents)?;
        file.flush()
    })();
    let outcome = written.and_then(|()| std::fs::rename(&tmp, path));
    if outcome.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    outcome
}

/// Reads a small file written by `write_private_file`: opened with
/// `O_NOFOLLOW`, a regular file owned by this user, at most 4 KiB. `None` for
/// anything else (missing, a symlink, foreign owner, not UTF-8).
#[cfg(unix)]
pub(crate) fn read_private_file(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != euid {
        return None;
    }
    let mut buf = String::new();
    file.take(PRIVATE_FILE_READ_CAP)
        .read_to_string(&mut buf)
        .ok()?;
    Some(buf)
}

/// Deletes a file (or a symlink, never its target). A missing file is fine.
pub(crate) fn remove_private_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Records that the user stopped the daemon: writes `<home>/stopped-by-user`
/// (0600, safe write). Best effort for the caller: a failure only means the
/// app may restart the daemon.
pub fn write_stop_marker(home: &Path) -> io::Result<()> {
    write_private_file(&home.join(STOPPED_BY_USER), b"1\n")
}

/// True when `<home>/stopped-by-user` exists (not followed if a symlink).
pub fn stop_marker_present(home: &Path) -> bool {
    std::fs::symlink_metadata(home.join(STOPPED_BY_USER)).is_ok()
}

/// Deletes `<home>/stopped-by-user`. A missing marker is fine.
pub fn clear_stop_marker(home: &Path) {
    remove_private_file(&home.join(STOPPED_BY_USER));
}

/// Name of the single-instance lock file in the bridge folder.
pub(crate) const DAEMON_LOCK: &str = "daemon.lock";

/// The one lock fd of this process. The daemon never closes it: a POSIX
/// `fcntl` lock is dropped when ANY fd to the file closes in the process, so
/// nothing else in the process may open `daemon.lock`.
#[cfg(unix)]
static HELD_LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

/// True when `HELD_LOCK` really holds an `fcntl` lock (false on a filesystem
/// with no locks: the fd is kept only to write the lock info).
#[cfg(unix)]
static LOCK_IS_REAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The fd of the lock file, when this process really holds the folder lock.
/// The in-place restart hands it to the new image (`upgrade::exec_in_place`).
#[cfg(unix)]
pub(crate) fn held_lock_fd() -> Option<std::os::unix::io::RawFd> {
    use std::os::unix::io::AsRawFd;
    if !LOCK_IS_REAL.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    HELD_LOCK.get().map(AsRawFd::as_raw_fd)
}

/// Sets or clears close-on-exec on the lock fd. Cleared only right before
/// the in-place restart `exec`s: the fd then survives into the new image and
/// the lock stays held. It is never cleared at any other time, so no child
/// process inherits it.
#[cfg(unix)]
pub(crate) fn set_lock_fd_inheritable(inherit: bool) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let Some(file) = HELD_LOCK.get() else {
        return Err(io::Error::other("no bridge folder lock is held"));
    };
    set_cloexec(file.as_raw_fd(), !inherit)
}

#[cfg(unix)]
fn set_cloexec(fd: std::os::unix::io::RawFd, on: bool) -> io::Result<()> {
    // SAFETY: `fcntl` on an fd number only reads or sets its flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let wanted = if on {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        if libc::fcntl(fd, libc::F_SETFD, wanted) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Takes over the lock fd an in-place restart left open. The lock itself
/// never moved: it belongs to this process and survives `exec` while the fd
/// stays open. Returns `None` unless the handoff is whole and checks out:
/// the fd is a regular file of this user, it is the very file `<dir>/daemon.lock`
/// names now (same device and inode, no symlink), and the pid in its lock
/// info is this process's pid (an `exec` keeps the pid). The file is read
/// through the fd itself: opening `daemon.lock` again, even once, would drop
/// the lock when that second fd closed.
#[cfg(unix)]
fn adopt_inherited_lock(dir: &Path) -> Option<std::fs::File> {
    let fd = crate::upgrade::reexec_info()?.lock_fd?;
    adopt_lock_fd(dir, fd)
}

/// `adopt_inherited_lock` for an explicit fd number. A refused fd that is
/// open stays open (closing it would drop the lock it may hold) but gets
/// close-on-exec at once, so it never leaks into a child process.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // `st_dev` and `st_ino` differ in width by platform
fn adopt_lock_fd(dir: &Path, fd: std::os::unix::io::RawFd) -> Option<std::fs::File> {
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::os::unix::io::FromRawFd;
    // SAFETY: only `fstat` runs on the fd before it is proven to be ours.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    // The fd is open. Whatever the checks below decide, it must not reach a
    // child process. Never close it here: that would drop the lock.
    let _ = set_cloexec(fd, true);
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let regular = (st.st_mode & libc::S_IFMT) == libc::S_IFREG;
    let named = std::fs::symlink_metadata(dir.join(DAEMON_LOCK)).ok()?;
    if !regular
        || st.st_uid != euid
        || !named.is_file()
        || named.dev() != st.st_dev as u64
        || named.ino() != st.st_ino as u64
    {
        return None;
    }
    // SAFETY: the fd is open (fstat succeeded) and nothing else in this
    // process owns it: it came through `exec` from the previous image. The
    // `ManuallyDrop` keeps a failed check from closing it.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let mut buf = [0u8; 4096];
    let n = file.read_at(&mut buf, 0).ok()?;
    let info: serde_json::Value = serde_json::from_slice(&buf[..n]).ok()?;
    if info.get("pid").and_then(serde_json::Value::as_u64) != Some(u64::from(std::process::id())) {
        return None;
    }
    set_cloexec(fd, true).ok()?;
    let file = std::mem::ManuallyDrop::into_inner(file);
    LOCK_IS_REAL.store(true, std::sync::atomic::Ordering::SeqCst);
    Some(file)
}

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
    // `turbofig js` refuses such a folder, so the daemon must too: it would
    // serve a folder no agent can use, and another account could queue jobs.
    check_not_shared_writable(dir)?;
    // After an in-place restart the lock is already held: adopt the open fd
    // instead of opening the file (see `adopt_inherited_lock`).
    if let Some(file) = adopt_inherited_lock(dir) {
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
            LOCK_IS_REAL.store(true, std::sync::atomic::Ordering::SeqCst);
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
            Some(e) if lock_errno_means_unsupported(e) => {
                // Keep the fd (no lock is held) so `write_lock_info` can
                // record `"locked":false` for `turbofig js`.
                let _ = HELD_LOCK.set(file);
                return Ok(LockOutcome::Unsupported);
            }
            _ => return Err(err),
        }
        if std::time::Instant::now() >= until {
            return Ok(LockOutcome::Busy);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Writes `{"pid":..,"requestTimeoutMs":..,"locked":..,"version":..,"buildId":..}`
/// into the lock file,
/// in place, through the held fd (no second fd is ever opened: that would
/// drop the lock). `locked` is false when the filesystem has no locks
/// (`LockOutcome::Unsupported`). The new body goes in first and the file is
/// then cut to its length, so a reader never sees an empty file. `turbofig js`
/// reads it to size its wait. Best effort.
#[cfg(unix)]
pub fn write_lock_info(request_timeout_ms: u64, locked: bool) {
    use std::os::unix::fs::FileExt;
    let Some(file) = HELD_LOCK.get() else {
        return;
    };
    // `version` and `buildId` let `turbofig js` see that the daemon is older
    // than itself without any network call.
    let mut body = serde_json::json!({
        "pid": std::process::id(),
        "requestTimeoutMs": request_timeout_ms,
        "locked": locked,
        "version": crate::mcp::reported_version(),
        "buildId": crate::upgrade::running_build_id(),
    })
    .to_string();
    body.push('\n');
    if file.write_all_at(body.as_bytes(), 0).is_ok() {
        let _ = file.set_len(body.len() as u64);
    }
}

/// What the daemon recorded in its lock file.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LockInfo {
    pub pid: Option<u32>,
    pub request_timeout: Option<std::time::Duration>,
    /// False when the daemon runs without a lock (a filesystem with no
    /// locks). A file without the field counts as locked.
    pub locked: bool,
    /// The daemon's version. Absent in the lock file of a daemon from before
    /// the lock info carried it.
    pub version: Option<String>,
    /// The build id the daemon runs as. Absent for the same reason.
    pub build_id: Option<String>,
}

#[cfg(unix)]
enum LockRead {
    /// No usable file (missing, a symlink, or owned by another user).
    Missing,
    /// The file is there but empty or not JSON yet.
    Unreadable,
    Info(LockInfo),
}

#[cfg(unix)]
fn read_lock_info_once(dir: &Path) -> LockRead {
    use std::io::Read;
    let Some(mut file) = open_lock_readonly(dir) else {
        return LockRead::Missing;
    };
    let mut buf = String::new();
    if (&mut file).take(4096).read_to_string(&mut buf).is_err() {
        return LockRead::Unreadable;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim()) else {
        return LockRead::Unreadable;
    };
    LockRead::Info(LockInfo {
        pid: v
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|p| u32::try_from(p).ok()),
        request_timeout: v
            .get("requestTimeoutMs")
            .and_then(serde_json::Value::as_u64)
            .filter(|ms| *ms > 0)
            .map(std::time::Duration::from_millis),
        locked: v
            .get("locked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        version: v
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        build_id: v
            .get("buildId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

/// How long a reader retries an empty or half-written lock file.
#[cfg(unix)]
const LOCK_INFO_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

/// The lock file content, if readable. Opens it read-only with `O_NOFOLLOW`
/// and requires a regular file owned by this user. An empty or unparseable
/// file is retried for up to 100 ms (the daemon may be writing it).
#[cfg(unix)]
pub(crate) fn read_lock_info(dir: &Path) -> Option<LockInfo> {
    let until = std::time::Instant::now() + LOCK_INFO_RETRY;
    loop {
        match read_lock_info_once(dir) {
            LockRead::Info(info) => return Some(info),
            LockRead::Missing => return None,
            LockRead::Unreadable => {}
        }
        if std::time::Instant::now() >= until {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The request timeout the daemon recorded in its lock file, if readable.
#[cfg(unix)]
pub(crate) fn read_lock_request_timeout(dir: &Path) -> Option<std::time::Duration> {
    read_lock_info(dir)?.request_timeout
}

/// True when `<dir>/daemon.lock` does not exist at all (not even as a
/// symlink). A daemon from before the lock existed never creates it.
#[cfg(unix)]
pub(crate) fn lock_file_absent(dir: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(dir.join(DAEMON_LOCK)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    )
}

/// The pid of a daemon that runs without a lock (its lock file says
/// `"locked":false`) and is still alive (`kill(pid, 0)`). `None` otherwise.
#[cfg(unix)]
pub(crate) fn unlocked_daemon_pid(dir: &Path) -> Option<u32> {
    let info = read_lock_info(dir)?;
    let pid = info.pid?;
    (!info.locked && pid_alive(pid)).then_some(pid)
}

/// True when a process with this pid exists (`kill(pid, 0)` succeeds).
#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
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

    #[test]
    fn lock_info_is_written_without_ever_being_empty_and_shrinks_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join(DAEMON_LOCK);
        // A longer old body, then a rewrite: no stale tail, valid JSON.
        std::fs::write(&f, "x".repeat(500)).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&f).unwrap();
        use std::os::unix::fs::FileExt;
        let body = "{\"pid\":1,\"requestTimeoutMs\":5,\"locked\":false}\n";
        file.write_all_at(body.as_bytes(), 0).unwrap();
        // Before the cut the file is never shorter than the new body.
        assert!(std::fs::metadata(&f).unwrap().len() >= body.len() as u64);
        file.set_len(body.len() as u64).unwrap();
        let info = read_lock_info(tmp.path()).unwrap();
        assert_eq!(info.pid, Some(1));
        assert!(!info.locked);
    }

    #[test]
    fn an_empty_lock_file_is_retried_until_content_arrives() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join(DAEMON_LOCK);
        std::fs::write(&f, "").unwrap();
        let writer = std::thread::spawn({
            let f = f.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(30));
                std::fs::write(&f, "{\"pid\":7,\"requestTimeoutMs\":9000}\n").unwrap();
            }
        });
        let info = read_lock_info(tmp.path()).expect("the retry must see the content");
        writer.join().unwrap();
        assert_eq!(
            info.request_timeout,
            Some(std::time::Duration::from_secs(9))
        );
        assert!(info.locked, "a missing locked field counts as locked");
    }

    #[test]
    fn an_unreadable_lock_file_gives_up_after_the_retry_window() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(DAEMON_LOCK), "").unwrap();
        let t = std::time::Instant::now();
        assert_eq!(read_lock_info(tmp.path()), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        // A missing file is not retried.
        let empty = tempfile::tempdir().unwrap();
        let t = std::time::Instant::now();
        assert_eq!(read_lock_info(empty.path()), None);
        assert!(t.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn lock_file_absent_and_unlocked_daemon_pid() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(lock_file_absent(tmp.path()));
        let f = tmp.path().join(DAEMON_LOCK);
        let me = std::process::id();
        std::fs::write(&f, format!("{{\"pid\":{me},\"locked\":false}}")).unwrap();
        assert!(!lock_file_absent(tmp.path()));
        assert_eq!(unlocked_daemon_pid(tmp.path()), Some(me));
        // locked:true is not an unlocked daemon.
        std::fs::write(&f, format!("{{\"pid\":{me},\"locked\":true}}")).unwrap();
        assert_eq!(unlocked_daemon_pid(tmp.path()), None);
        // A dead pid is not alive.
        std::fs::write(&f, "{\"pid\":2147483646,\"locked\":false}").unwrap();
        assert_eq!(unlocked_daemon_pid(tmp.path()), None);
    }

    #[test]
    fn the_daemon_refuses_a_group_writable_bridge_folder() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("b");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o775)).unwrap();
        // Custom folder (own_mode false): the mode is kept, so it is refused.
        let e = prepare_bridge_dirs(&d, false).unwrap_err();
        assert!(is_refusal(&e), "{e}");
        assert!(e.to_string().contains("other accounts can write"), "{e}");
        let e = lock_bridge_dir(&d, false, std::time::Duration::ZERO).unwrap_err();
        assert!(is_refusal(&e), "{e}");
        assert!(!d.join(DAEMON_LOCK).exists() || !bridge_lock_held(&d));
        // The default home is tightened to 0700 first, so it passes.
        assert!(prepare_bridge_dirs(&d, true).is_ok());
    }

    fn has_cloexec(fd: std::os::unix::io::RawFd) -> bool {
        // SAFETY: `fcntl(F_GETFD)` only reads the flags of an fd number.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn a_refused_inherited_lock_fd_stays_open_but_gets_close_on_exec() {
        use std::os::unix::io::AsRawFd;
        let tmp = tempfile::tempdir().unwrap();
        // The folder has a daemon.lock that is NOT the file behind the fd.
        std::fs::write(tmp.path().join(DAEMON_LOCK), "{}").unwrap();
        let other = tmp.path().join("other");
        std::fs::write(&other, "{}").unwrap();
        let file = std::fs::File::open(&other).unwrap();
        let fd = file.as_raw_fd();
        set_cloexec(fd, false).unwrap();
        assert!(!has_cloexec(fd));
        assert!(adopt_lock_fd(tmp.path(), fd).is_none());
        assert!(has_cloexec(fd), "a refused fd must not leak into children");
        // Still open: closing it would have dropped a held lock.
        assert!(std::fs::File::metadata(&file).is_ok());
    }

    #[test]
    fn a_refused_fd_with_the_wrong_pid_also_gets_close_on_exec() {
        use std::os::unix::io::AsRawFd;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(DAEMON_LOCK);
        std::fs::write(&path, "{\"pid\":1}").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd();
        set_cloexec(fd, false).unwrap();
        assert!(adopt_lock_fd(tmp.path(), fd).is_none());
        assert!(has_cloexec(fd));
    }

    #[test]
    fn a_private_file_is_written_0600_and_read_back_and_a_symlink_is_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m");
        write_private_file(&path, b"abc").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(read_private_file(&path).as_deref(), Some("abc"));
        assert!(!tmp.path().join("m.tmp").exists());
        // A symlink planted at the tmp name is replaced, never written through.
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        symlink(&victim, tmp.path().join("m.tmp")).unwrap();
        write_private_file(&path, b"def").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(read_private_file(&path).as_deref(), Some("def"));
        // A symlink at the file itself reads as nothing.
        let link = tmp.path().join("link");
        symlink(&victim, &link).unwrap();
        assert_eq!(read_private_file(&link), None);
        assert_eq!(read_private_file(&tmp.path().join("missing")), None);
    }

    #[test]
    fn the_stop_marker_is_written_seen_and_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!stop_marker_present(tmp.path()));
        write_stop_marker(tmp.path()).unwrap();
        assert!(stop_marker_present(tmp.path()));
        assert_eq!(mode(&tmp.path().join(STOPPED_BY_USER)), 0o600);
        clear_stop_marker(tmp.path());
        assert!(!stop_marker_present(tmp.path()));
        clear_stop_marker(tmp.path()); // a missing marker is fine
    }
}
