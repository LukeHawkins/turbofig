//! Upgrade convergence: after `brew upgrade turbofig`, the daemon, the app
//! and the plugin files all move to the new build with no user action.
//!
//! Three pieces live here:
//!
//! - Build ids. A build id is a fingerprint of a binary's exact bytes
//!   (`build_id_for_bytes`). The app bundle stores the build id of the
//!   binary it was built from in `Info.plist` (`TurbofigBuildId`).
//! - The upgrade watcher (`run_upgrade_watcher`). Every daemon, supervised
//!   or not, watches the stable Homebrew binary path. When the file there
//!   has another build id than the running image, the daemon drains and
//!   restarts in place: it `exec`s the stable binary with the same pid. The
//!   bridge folder lock is a POSIX `fcntl` lock, which belongs to the
//!   process and survives `exec` while its fd stays open, so no second
//!   daemon can take the folder during the restart. The new image adopts
//!   the inherited fd (`secure_fs::lock_bridge_dir`).
//! - Pure decision functions the app, the bare `turbofig` command and `js`
//!   share, so each is unit tested with no real clock, process or window.

use crate::state::AppState;
use crate::supervisor::{should_log_binary_gone, wait_for_drain};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Env var the old image sets for the new one: the build id it ran as. The
/// new image never `exec`s again when it runs as that same build (the loop
/// guard).
pub const ENV_REEXEC_FROM: &str = "TURBOFIG_REEXEC_FROM";
/// Env var the old image sets for the new one: the fd number of the open
/// `daemon.lock` file that still holds the bridge folder lock.
pub const ENV_INHERITED_LOCK_FD: &str = "TURBOFIG_INHERITED_LOCK_FD";

/// How often the watcher checks the stable binary.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
/// Pause between "a change was seen" and acting on it, so a file that is
/// still being written is never run.
const SETTLE: Duration = Duration::from_secs(1);
/// Shortest cap on the watcher's wait for in-flight jobs. The real cap is
/// `drain_cap`: a job can run for the whole request timeout, which an env var
/// can raise to 600 s.
const DRAIN_MAX_WAIT: Duration = Duration::from_secs(60);
/// Added to the request timeout in `drain_cap`: a result write or a response
/// can land a few seconds after the job's own timeout.
const DRAIN_MARGIN: Duration = Duration::from_secs(15);
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Grace after the job count reaches 0: a response or a result-file rename
/// can be a few scheduler ticks from landing.
const EXIT_GRACE: Duration = Duration::from_millis(250);
/// Longest the new binary may take to answer `--version` in the preflight.
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(10);

/// Longest a draining daemon waits for in-flight jobs before it restarts
/// anyway: 60 s, or the request timeout plus a margin when that is longer. A
/// job allowed to run for 600 s is never cut off by an upgrade.
pub fn drain_cap(request_timeout: Duration) -> Duration {
    DRAIN_MAX_WAIT.max(request_timeout + DRAIN_MARGIN)
}

/// Where a Homebrew `turbofig` lives when the daemon cannot learn the path
/// from the app bundle (a bundle written before `TurbofigSourcePath`).
const FALLBACK_STABLE_PATHS: [&str; 2] = ["/opt/homebrew/bin/turbofig", "/usr/local/bin/turbofig"];

// ---------------------------------------------------------------- build ids

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a (64-bit) over a stream of bytes, plus the byte count. Not
/// cryptographic: it answers "is this the same build", nothing more. The
/// algorithm is fixed by its definition, so the same bytes give the same id on
/// every toolchain (`std::hash::DefaultHasher` makes no such promise).
struct Fingerprint {
    hash: u64,
    len: u64,
}

impl Fingerprint {
    fn new() -> Self {
        Self {
            hash: FNV_OFFSET,
            len: 0,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.hash ^= u64::from(b);
            self.hash = self.hash.wrapping_mul(FNV_PRIME);
        }
        self.len += bytes.len() as u64;
    }

    fn finish(&self) -> String {
        format!("{:016x}-{}", self.hash, self.len)
    }
}

/// The build id of `bytes`. `app_bundle` stamps this value into the bundle's
/// `Info.plist` (`TurbofigBuildId`).
pub fn build_id_for_bytes(bytes: &[u8]) -> String {
    let mut f = Fingerprint::new();
    f.update(bytes);
    f.finish()
}

/// The build id of the file at `path`, read in chunks (a binary is many
/// megabytes). Equals `build_id_for_bytes` of the same content.
pub fn build_id_for_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut f = Fingerprint::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(f.finish());
        }
        f.update(&buf[..n]);
    }
}

/// The string after `<key>{key}</key>` in a plist's XML. Only ever reads a
/// plist this crate wrote, so this narrow scan is enough.
pub fn plist_string(xml: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let after_key = &xml[xml.find(&key_tag)? + key_tag.len()..];
    let value_start = after_key.find("<string>")? + "<string>".len();
    let value_end = after_key[value_start..].find("</string>")?;
    Some(unescape_xml(
        &after_key[value_start..value_start + value_end],
    ))
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `<X>.app/Contents/Info.plist` for an exe at `<X>.app/Contents/MacOS/<name>`.
fn bundle_plist_path_of_exe(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    Some(contents.join("Info.plist"))
}

/// A string key of the `Info.plist` of the app bundle that `exe` sits in, or
/// `None` when `exe` is not inside a bundle or the key is absent.
pub fn bundle_plist_string_of_exe(exe: &Path, key: &str) -> Option<String> {
    let plist = std::fs::read_to_string(bundle_plist_path_of_exe(exe)?).ok()?;
    plist_string(&plist, key)
}

fn current_exe_resolved() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.canonicalize().unwrap_or(exe))
}

/// The build id this process runs as. Inside an app bundle that is the id the
/// bundle recorded for the binary it copied (`TurbofigBuildId`): the bundle's
/// own copy is re-signed after the copy, so hashing it would never match the
/// Homebrew binary it came from. Anywhere else it is the hash of the running
/// binary file, read once at startup.
pub fn running_build_id() -> Option<&'static str> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| {
        let exe = current_exe_resolved()?;
        bundle_plist_string_of_exe(&exe, "TurbofigBuildId").or_else(|| build_id_for_file(&exe).ok())
    })
    .as_deref()
}

// ------------------------------------------------------------ exec handoff

/// What an `exec` restart handed to this image through the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReexecInfo {
    /// The build id the previous image ran as.
    pub from_build_id: String,
    /// The fd of the open `daemon.lock` file that holds the folder lock.
    pub lock_fd: Option<i32>,
}

/// Parses the two handoff variables. `None` unless a plausible previous
/// build id is present. A bad fd number gives `lock_fd: None`.
pub fn parse_reexec_env(from: Option<&str>, fd: Option<&str>) -> Option<ReexecInfo> {
    let from = from?.trim();
    let plausible = !from.is_empty()
        && from.len() <= 64
        && from.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    if !plausible {
        return None;
    }
    let lock_fd = fd
        .and_then(|v| v.trim().parse::<i32>().ok())
        .filter(|n| *n > 2);
    Some(ReexecInfo {
        from_build_id: from.to_owned(),
        lock_fd,
    })
}

static REEXEC: OnceLock<Option<ReexecInfo>> = OnceLock::new();

/// Reads and REMOVES the handoff variables. Call once at the very start of
/// `main`, before any thread exists, so a child process never inherits them.
pub fn capture_reexec_env() {
    let from = std::env::var(ENV_REEXEC_FROM).ok();
    let fd = std::env::var(ENV_INHERITED_LOCK_FD).ok();
    std::env::remove_var(ENV_REEXEC_FROM);
    std::env::remove_var(ENV_INHERITED_LOCK_FD);
    let _ = REEXEC.set(parse_reexec_env(from.as_deref(), fd.as_deref()));
}

/// The handoff this image was started with, if it came from an `exec` restart.
pub fn reexec_info() -> Option<&'static ReexecInfo> {
    REEXEC.get().and_then(Option::as_ref)
}

/// True when this image runs as the very build it was `exec`ed from: the
/// restart changed nothing. The watcher then never `exec`s again, so a
/// launcher that always runs the same build cannot loop.
pub fn no_progress_reexec(running_id: Option<&str>, info: Option<&ReexecInfo>) -> bool {
    match (running_id, info) {
        (Some(running), Some(info)) => running == info.from_build_id,
        _ => false,
    }
}

// ----------------------------------------------------------- watch decision

/// What a stat of the binary says: where it resolves, which file it is, how
/// big and how old.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySig {
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
}

/// The signature of the file `path` resolves to now. `None` when it cannot be
/// resolved (mid-upgrade, or after `brew uninstall`): the caller treats that
/// as "cannot tell", never as a change.
pub fn signature_of(path: &Path) -> Option<BinarySig> {
    let resolved = path.canonicalize().ok()?;
    let meta = std::fs::metadata(&resolved).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(BinarySig {
        path: resolved,
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime_sec: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
    })
}

/// What one check of the binary decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The binary could not be resolved or read this time.
    Unresolved,
    /// Same build as the running image.
    Unchanged,
    /// Another build, but restarting is off (the loop guard).
    Suppressed { build_id: String },
    /// Another build: restart onto it.
    Upgrade { build_id: String, sig: BinarySig },
}

/// The pure upgrade decision. It remembers the last signature it fully
/// checked, so the binary is read and hashed only when its path, inode, size
/// or mtime changed, not on every poll.
#[derive(Debug)]
pub struct Watcher {
    running_id: String,
    last_sig: Option<BinarySig>,
    restart_allowed: bool,
    /// The signature of a build that failed to restart onto, and how many
    /// times it failed. Once it reaches `MAX_RESTART_ATTEMPTS` the build is
    /// skipped (no hash, no preflight) until its signature changes.
    failed: Option<(BinarySig, u32)>,
}

/// Failed restarts onto one build signature before the watcher stops trying.
/// More than one: the first run of a fresh binary can be slow (Gatekeeper
/// scans it), so one slow `--version` must not park a good build for good.
pub const MAX_RESTART_ATTEMPTS: u32 = 3;

impl Watcher {
    pub fn new(running_id: String, restart_allowed: bool) -> Self {
        Self {
            running_id,
            last_sig: None,
            restart_allowed,
            failed: None,
        }
    }

    /// Records that restarting onto the build with signature `sig` failed (the
    /// preflight or the `exec`). Returns true when this was the last attempt:
    /// the build is now skipped until its signature changes.
    pub fn record_failure(&mut self, sig: &BinarySig) -> bool {
        let count = match &self.failed {
            Some((s, n)) if s == sig => n + 1,
            _ => 1,
        };
        self.failed = Some((sig.clone(), count));
        count >= MAX_RESTART_ATTEMPTS
    }

    /// Decides for one poll. `current` is the fresh signature (or `None`);
    /// `hash` returns the build id of the file at a path (or `None`).
    pub fn check(
        &mut self,
        current: Option<BinarySig>,
        hash: impl FnOnce(&Path) -> Option<String>,
    ) -> Verdict {
        let Some(sig) = current else {
            return Verdict::Unresolved;
        };
        if self.last_sig.as_ref() == Some(&sig) {
            return Verdict::Unchanged;
        }
        // A build that already failed its attempts is not hashed or checked
        // again until its signature changes.
        if matches!(&self.failed, Some((s, n)) if s == &sig && *n >= MAX_RESTART_ATTEMPTS) {
            return Verdict::Unchanged;
        }
        let Some(build_id) = hash(&sig.path) else {
            return Verdict::Unresolved;
        };
        if build_id == self.running_id {
            // The same build, whatever the path, inode or mtime say (a
            // reinstall of the same bytes): nothing to restart for.
            self.last_sig = Some(sig);
            return Verdict::Unchanged;
        }
        if !self.restart_allowed {
            self.last_sig = Some(sig);
            return Verdict::Suppressed { build_id };
        }
        Verdict::Upgrade { build_id, sig }
    }
}

/// The first existing path of `source` (the path the bundle recorded), then
/// the Homebrew fallbacks.
pub fn pick_existing(source: Option<&Path>, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    source
        .into_iter()
        .map(Path::to_path_buf)
        .chain(fallbacks.iter().cloned())
        .find(|p| p.exists())
}

fn fallback_paths() -> Vec<PathBuf> {
    FALLBACK_STABLE_PATHS.iter().map(PathBuf::from).collect()
}

/// The `turbofig` binary a daemon (or the app) started from the app bundle
/// should run instead: the source path the bundle recorded, else the Homebrew
/// path. `None` when `exe` is not inside a bundle or no such binary exists.
pub fn source_binary_for_bundle_exe(exe: &Path) -> Option<PathBuf> {
    bundle_plist_path_of_exe(exe)?;
    let recorded = bundle_plist_string_of_exe(exe, "TurbofigSourcePath").map(PathBuf::from);
    pick_existing(recorded.as_deref(), &fallback_paths())
}

/// The binary the watcher compares the running image against: the stable
/// Homebrew path, or for a daemon that runs from the app bundle the source
/// path the bundle recorded.
pub fn watch_path() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Ok(p) = std::env::var("TURBOFIG_TEST_UPGRADE_WATCH") {
        return Some(PathBuf::from(p));
    }
    let exe = current_exe_resolved()?;
    if bundle_plist_path_of_exe(&exe).is_some() {
        return source_binary_for_bundle_exe(&exe);
    }
    Some(crate::launchd::stable_binary_path(&exe))
}

/// The binary a process should start a new daemon from: the Homebrew binary
/// when the running binary is the app bundle's copy, else the running binary.
/// A daemon started from the stable path restarts onto the next upgrade by
/// itself; one started from the bundle copy would first have to move.
pub fn daemon_start_binary() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let resolved = exe.canonicalize().unwrap_or_else(|_| exe.clone());
    Ok(source_binary_for_bundle_exe(&resolved).unwrap_or(exe))
}

// -------------------------------------------------------------- the watcher

fn check_interval() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("TURBOFIG_TEST_UPGRADE_CHECK_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    CHECK_INTERVAL
}

/// The grace after the drain. A debug build honours
/// `TURBOFIG_TEST_UPGRADE_EXIT_GRACE_MS`, so a test can hold the window in
/// which a `/control restart` can still join the drain. Release builds always
/// use `EXIT_GRACE`.
fn exit_grace() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("TURBOFIG_TEST_UPGRADE_EXIT_GRACE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    EXIT_GRACE
}

fn settle_delay() -> Duration {
    #[cfg(debug_assertions)]
    if std::env::var("TURBOFIG_TEST_UPGRADE_CHECK_MS").is_ok() {
        return Duration::from_millis(50);
    }
    SETTLE
}

/// Runs the new binary once with `--version`. A binary that cannot start
/// (a bad signature, a half-written file) is never `exec`ed into: that would
/// kill the daemon and leave nothing to restart it.
async fn preflight(target: &Path) -> bool {
    let mut cmd = tokio::process::Command::new(target);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    matches!(
        tokio::time::timeout(PREFLIGHT_TIMEOUT, child.wait()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Replaces this process image with `<target> serve`, same pid, same env plus
/// the handoff variables, same open `daemon.lock` fd (the folder lock stays
/// held). Returns only on failure, with the fd made close-on-exec again.
pub fn exec_in_place(target: &Path, running_id: &str) -> std::io::Error {
    let mut cmd = std::process::Command::new(target);
    cmd.arg("serve").env(ENV_REEXEC_FROM, running_id);
    let mut inherited = false;
    if let Some(fd) = crate::secure_fs::held_lock_fd() {
        if crate::secure_fs::set_lock_fd_inheritable(true).is_ok() {
            cmd.env(ENV_INHERITED_LOCK_FD, fd.to_string());
            inherited = true;
        }
    }
    let err = cmd.exec();
    if inherited {
        let _ = crate::secure_fs::set_lock_fd_inheritable(false);
    }
    err
}

/// The upgrade watcher. Runs in every daemon mode. On a new build at the
/// stable path: stop claiming jobs, wait for in-flight jobs, then `exec` the
/// new binary in place. If `exec` fails the daemon accepts work again; under
/// launchd it exits with the restart code instead (launchd starts the new
/// build). A `/control stop` that arrives during the drain always wins.
pub async fn run_upgrade_watcher(state: Arc<AppState>) {
    let Some(running_id) = running_build_id().map(str::to_owned) else {
        eprintln!("turbofig daemon: cannot read its own build id; not watching for upgrades");
        return;
    };
    let allowed = !no_progress_reexec(Some(&running_id), reexec_info());
    if !allowed {
        eprintln!(
            "turbofig daemon: restarted into the same build ({running_id}); \
             not restarting in place again"
        );
    }
    let mut watcher = Watcher::new(running_id.clone(), allowed);
    let mut interval = tokio::time::interval(check_interval());
    interval.tick().await; // the first tick fires at once; consume it
    let mut unresolved: u32 = 0;
    let mut logged_suppressed = false;

    loop {
        interval.tick().await;
        let Some(path) = watch_path() else {
            continue;
        };
        let sig = signature_of(&path);
        let verdict = watcher.check(sig, |p| build_id_for_file(p).ok());
        if verdict == Verdict::Unresolved {
            unresolved += 1;
            if should_log_binary_gone(unresolved) {
                eprintln!(
                    "turbofig daemon: the turbofig binary at {} is gone or unreadable; \
                     run `turbofig uninstall` or reinstall",
                    path.display()
                );
            }
            continue;
        }
        unresolved = 0;
        match verdict {
            Verdict::Unresolved | Verdict::Unchanged => {}
            Verdict::Suppressed { build_id } => {
                if !logged_suppressed {
                    logged_suppressed = true;
                    eprintln!(
                        "turbofig daemon: a new build ({build_id}) is installed; \
                         not restarting in place (loop guard)"
                    );
                }
            }
            Verdict::Upgrade { build_id, sig } => {
                // Returns only when the restart did not happen.
                if restart_onto(&state, &running_id, &build_id, &sig).await
                    == RestartOutcome::Failed
                    && watcher.record_failure(&sig)
                {
                    eprintln!(
                        "turbofig daemon: giving up on build {build_id} at {} until the file changes",
                        sig.path.display()
                    );
                }
            }
        }
    }
}

/// Why `restart_onto` came back (it never returns on success).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartOutcome {
    /// Nothing wrong with the build: look again on the next poll (the file
    /// was still changing, or another drain owns the exit).
    Retry,
    /// The build failed the preflight or the `exec`: count it.
    Failed,
}

/// Restores SIGPIPE to "ignored", the state Rust's runtime sets at start. A
/// failed `exec` can leave the default action: `Command::exec` resets SIGPIPE
/// to the default before it calls `execvp` and does not undo that on failure.
/// A daemon that keeps serving would then die on the first write to a closed
/// socket. Restoring it is safer than exiting here: the daemon holds the
/// folder lock and in-flight state, and an unsupervised daemon has no
/// launchd to bring it back.
fn restore_sigpipe_ignore() {
    // SAFETY: `signal` with SIG_IGN installs no handler code.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Drains and restarts onto the new build. Returns only when the restart did
/// not happen (the daemon runs on); never returns on success.
async fn restart_onto(
    state: &Arc<AppState>,
    running_id: &str,
    new_build_id: &str,
    sig: &BinarySig,
) -> RestartOutcome {
    tokio::time::sleep(settle_delay()).await;
    if signature_of(&sig.path).as_ref() != Some(sig) {
        return RestartOutcome::Retry; // still being written: look again on the next poll
    }
    if !preflight(&sig.path).await {
        eprintln!(
            "turbofig daemon: the new build at {} did not start with --version; not restarting",
            sig.path.display()
        );
        return RestartOutcome::Failed;
    }
    if !state.try_begin_draining() {
        // A /control stop or restart is already draining: it ends the process.
        return RestartOutcome::Retry;
    }
    println!(
        "turbofig daemon: detected an upgrade ({running_id} -> {new_build_id}); draining and restarting"
    );
    let cap = drain_cap(state.request_timeout);
    let drained = wait_for_drain(|| state.jobs_in_flight(), cap, DRAIN_POLL_INTERVAL).await;
    if !drained {
        eprintln!(
            "turbofig daemon: {} job(s) still in flight after {cap:?}; restarting anyway",
            state.jobs_in_flight()
        );
    }
    tokio::time::sleep(exit_grace()).await;
    if state.stop_requested() {
        std::process::exit(0);
    }
    let err = exec_in_place(&sig.path, running_id);
    restore_sigpipe_ignore();
    eprintln!(
        "turbofig daemon: could not start {}: {err}",
        sig.path.display()
    );
    if crate::supervisor::is_supervised() {
        // launchd restarts the daemon on a non-zero exit.
        std::process::exit(crate::supervisor::SUPERVISED_RESTART_EXIT_CODE);
    }
    // A stop that came in during the failed `exec` still wins.
    if state.stop_requested() {
        std::process::exit(0);
    }
    // A `/control restart` that joined this drain got a `202` and was not
    // lost: exit now. Nothing supervises this daemon, so the caller that
    // asked (`turbofig js`, `turbofig mcp`, the app) starts the next one,
    // exactly as after a normal unsupervised restart.
    if crate::control::take_restart_during_drain() {
        eprintln!("turbofig daemon: honoring the /control restart that arrived during the drain");
        std::process::exit(0);
    }
    state.set_draining(false);
    RestartOutcome::Failed
}

// ----------------------------------------------------- app and CLI decisions

/// True when the running app must relaunch: the build id its bundle records
/// on disk now differs from the one it launched with. An unknown id on
/// either side never relaunches (it cannot be told from "unchanged").
pub fn app_should_relaunch(launched_with: Option<&str>, on_disk: Option<&str>) -> bool {
    matches!((launched_with, on_disk), (Some(a), Some(b)) if a != b)
}

/// Extra time the bare `turbofig` command waits for the app after the app's
/// own stop-and-confirm sequence ends, for the process exit itself.
pub const APP_QUIT_EXTRA_WAIT: Duration = Duration::from_secs(3);

/// How long the bare `turbofig` command waits for a running app to quit,
/// given the app's own quit deadline (`menu_bar::QUIT_WAIT_DEADLINE`: it
/// waits for the daemon to stop, then exits).
pub fn app_quit_wait(app_quit_deadline: Duration) -> Duration {
    app_quit_deadline + APP_QUIT_EXTRA_WAIT
}

/// How the bare `turbofig` command opens `turbofig.app`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// `open <bundle>`: starts the app, or activates one that already runs.
    Plain,
    /// `open -n <bundle>`: always a new instance. Used after a refresh asked
    /// the old instance to quit: a plain `open` could re-activate the old
    /// (exiting) instance and leave no app.
    NewInstance,
}

/// `NewInstance` when a refresh just replaced the bundle and quit an app.
pub fn open_mode_after_refresh(bundle_replaced: bool, app_was_running: bool) -> OpenMode {
    if bundle_replaced && app_was_running {
        OpenMode::NewInstance
    } else {
        OpenMode::Plain
    }
}

/// True when the headless autostart plist exists in the LaunchAgents folder.
pub fn headless_plist_exists() -> bool {
    crate::launchd::launch_agents_dir_from_env()
        .join(crate::launchd::plist_file_name())
        .exists()
}

/// True when the headless launchd service owns the daemon of this bridge
/// folder: its plist exists and the folder is the default one (the plist's
/// daemon serves that folder). The same rule `turbofig js` uses.
pub fn headless_service_owns_daemon() -> bool {
    service_owns_daemon(headless_plist_exists(), &crate::config::bridge_dir_custom())
}

/// The pure rule: the plist exists and the bridge folder is the default one
/// (`bridge_dir_custom` is empty for it).
pub fn service_owns_daemon(plist_exists: bool, custom_bridge_dir: &str) -> bool {
    plist_exists && custom_bridge_dir.is_empty()
}

/// Asks launchd to start the headless service when it is not running
/// (`launchctl kickstart`, no `-k`: a running service is left alone). Best
/// effort. True when launchctl accepted the request.
pub fn kickstart_headless_service() -> bool {
    #[cfg(target_os = "macos")]
    if let Ok(uid) = crate::launchd::current_uid() {
        return std::process::Command::new("/bin/launchctl")
            .args(["kickstart", &crate::launchd::service_target(&uid)])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    }
    false
}

/// How long a caller that asked the daemon to restart waits for it to leave:
/// at least `base`, and long enough for the daemon's own drain
/// (`drain_cap`) when the lock info recorded a long request timeout.
pub fn restart_wait(base: Duration, request_timeout: Option<Duration>) -> Duration {
    request_timeout.map_or(base, |t| base.max(drain_cap(t) + DRAIN_MARGIN))
}

/// True when the lock info now shows that a restart the caller asked for took
/// effect without the lock ever being released: the daemon restarted in place
/// (the upgrade watcher drained first and `exec`ed, keeping the lock), so its
/// new image rewrote the lock info with another build id, or a version that
/// is no longer older than `own_version`. Callers stop waiting for the lock to
/// go away when this is true. `asked_build_id` is the build id the lock info
/// held when the restart was asked.
pub(crate) fn restart_took_effect(
    asked_build_id: Option<&str>,
    now: Option<&crate::secure_fs::LockInfo>,
    own_version: &str,
) -> bool {
    let Some(now) = now else {
        return false;
    };
    if matches!((asked_build_id, now.build_id.as_deref()), (Some(a), Some(n)) if a != n) {
        return true;
    }
    // A version that is missing or cannot be parsed says nothing: it must
    // never count as "no longer older" (`daemon_needs_restart` treats an
    // unparseable version as "cannot tell", which is not a restart).
    now.version
        .as_deref()
        .is_some_and(|v| semver::Version::parse(v).is_ok())
        && !daemon_needs_restart(now.version.as_deref(), own_version)
}

/// True when the lock info a daemon recorded says it is older than `own`, or
/// does not say at all (a daemon from before the lock info carried a version).
/// An unparseable version on either side never counts as older.
pub fn daemon_needs_restart(lock_version: Option<&str>, own_version: &str) -> bool {
    let Some(theirs) = lock_version else {
        return true;
    };
    match (
        semver::Version::parse(theirs),
        semver::Version::parse(own_version),
    ) {
        (Ok(theirs), Ok(own)) => theirs < own,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(path: &str, ino: u64, size: u64, mtime: i64) -> BinarySig {
        BinarySig {
            path: PathBuf::from(path),
            dev: 1,
            ino,
            size,
            mtime_sec: mtime,
            mtime_nsec: 0,
        }
    }

    #[test]
    fn build_id_depends_on_content_and_is_stable() {
        assert_eq!(build_id_for_bytes(b"same"), build_id_for_bytes(b"same"));
        assert_ne!(build_id_for_bytes(b"one"), build_id_for_bytes(b"two"));
        // A fixed vector: the id must never change across toolchains.
        assert_eq!(build_id_for_bytes(b""), "cbf29ce484222325-0");
        assert_eq!(build_id_for_bytes(b"a"), "af63dc4c8601ec8c-1");
    }

    #[test]
    fn build_id_for_file_equals_build_id_for_bytes_across_chunk_sizes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bin");
        let bytes: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).expect("write");
        assert_eq!(
            build_id_for_file(&path).expect("hash"),
            build_id_for_bytes(&bytes)
        );
        assert!(build_id_for_file(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn plist_string_reads_and_unescapes_a_value() {
        let xml =
            "<dict><key>A</key><string>x &amp; y</string><key>B</key><string>2</string></dict>";
        assert_eq!(plist_string(xml, "A").as_deref(), Some("x & y"));
        assert_eq!(plist_string(xml, "B").as_deref(), Some("2"));
        assert_eq!(plist_string(xml, "C"), None);
    }

    #[test]
    fn bundle_plist_values_are_read_for_an_exe_inside_a_bundle_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contents = dir.path().join("turbofig.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).expect("dirs");
        std::fs::write(
            contents.join("Info.plist"),
            "<key>TurbofigBuildId</key><string>abc-1</string>",
        )
        .expect("plist");
        let inside = contents.join("MacOS/turbofig");
        assert_eq!(
            bundle_plist_string_of_exe(&inside, "TurbofigBuildId").as_deref(),
            Some("abc-1")
        );
        assert_eq!(bundle_plist_string_of_exe(&inside, "Missing"), None);
        let outside = dir.path().join("bin/turbofig");
        assert_eq!(
            bundle_plist_string_of_exe(&outside, "TurbofigBuildId"),
            None
        );
    }

    #[test]
    fn reexec_env_needs_a_plausible_build_id_and_a_real_fd() {
        assert_eq!(parse_reexec_env(None, Some("5")), None);
        assert_eq!(parse_reexec_env(Some(""), Some("5")), None);
        assert_eq!(parse_reexec_env(Some("not hex!"), Some("5")), None);
        let ok = parse_reexec_env(Some("00ff-12"), Some("7")).expect("parsed");
        assert_eq!(ok.from_build_id, "00ff-12");
        assert_eq!(ok.lock_fd, Some(7));
        // A bad or standard-stream fd number is dropped, not trusted.
        assert_eq!(
            parse_reexec_env(Some("00ff-12"), Some("x"))
                .unwrap()
                .lock_fd,
            None
        );
        assert_eq!(
            parse_reexec_env(Some("00ff-12"), Some("1"))
                .unwrap()
                .lock_fd,
            None
        );
        assert_eq!(
            parse_reexec_env(Some("00ff-12"), None).unwrap().lock_fd,
            None
        );
    }

    #[test]
    fn the_loop_guard_fires_only_when_the_restart_changed_nothing() {
        let info = parse_reexec_env(Some("aa-1"), Some("5"));
        assert!(no_progress_reexec(Some("aa-1"), info.as_ref()));
        assert!(!no_progress_reexec(Some("bb-1"), info.as_ref()));
        assert!(!no_progress_reexec(Some("aa-1"), None));
        assert!(!no_progress_reexec(None, info.as_ref()));
    }

    #[test]
    fn an_unresolved_binary_is_never_an_upgrade() {
        let mut w = Watcher::new("aa-1".into(), true);
        assert_eq!(w.check(None, |_| panic!("no hash")), Verdict::Unresolved);
        // A file that resolves but cannot be hashed is also "cannot tell".
        let s = sig("/b", 1, 10, 1);
        assert_eq!(w.check(Some(s), |_| None), Verdict::Unresolved);
    }

    #[test]
    fn a_new_build_id_is_an_upgrade_and_the_same_one_is_not() {
        let mut w = Watcher::new("aa-1".into(), true);
        let s = sig(
            "/opt/homebrew/Cellar/turbofig/0.2.1/bin/turbofig",
            9,
            100,
            5,
        );
        assert_eq!(
            w.check(Some(s.clone()), |_| Some("bb-2".into())),
            Verdict::Upgrade {
                build_id: "bb-2".into(),
                sig: s
            }
        );
        let same = sig("/opt/homebrew/Cellar/turbofig/0.2.0/bin/turbofig", 3, 90, 2);
        assert_eq!(
            w.check(Some(same), |_| Some("aa-1".into())),
            Verdict::Unchanged
        );
    }

    #[test]
    fn a_path_change_with_the_same_bytes_is_not_an_upgrade() {
        // `brew reinstall` of the same build moves the target path only.
        let mut w = Watcher::new("aa-1".into(), true);
        let moved = sig("/c/0.2.0_1/bin/turbofig", 4, 100, 7);
        assert_eq!(
            w.check(Some(moved), |_| Some("aa-1".into())),
            Verdict::Unchanged
        );
    }

    #[test]
    fn a_same_path_replacement_is_found_by_inode_size_or_mtime() {
        let mut w = Watcher::new("aa-1".into(), true);
        let base = sig("/opt/homebrew/bin/turbofig", 1, 100, 10);
        assert_eq!(
            w.check(Some(base.clone()), |_| Some("aa-1".into())),
            Verdict::Unchanged
        );
        // The file is hashed only when its signature changed.
        assert_eq!(
            w.check(Some(base.clone()), |_| panic!("not hashed again")),
            Verdict::Unchanged
        );
        for changed in [
            BinarySig {
                ino: 2,
                ..base.clone()
            },
            BinarySig {
                size: 101,
                ..base.clone()
            },
            BinarySig {
                mtime_sec: 11,
                ..base.clone()
            },
            BinarySig {
                mtime_nsec: 5,
                ..base.clone()
            },
        ] {
            let mut w2 = Watcher::new("aa-1".into(), true);
            w2.check(Some(base.clone()), |_| Some("aa-1".into()));
            assert!(
                matches!(
                    w2.check(Some(changed), |_| Some("cc-3".into())),
                    Verdict::Upgrade { .. }
                ),
                "a changed signature with another build id must upgrade"
            );
        }
    }

    #[test]
    fn the_loop_guard_turns_an_upgrade_into_a_suppressed_verdict() {
        let mut w = Watcher::new("aa-1".into(), false);
        let s = sig("/b", 1, 10, 1);
        assert_eq!(
            w.check(Some(s.clone()), |_| Some("bb-2".into())),
            Verdict::Suppressed {
                build_id: "bb-2".into()
            }
        );
        // The suppressed signature is remembered: no repeated hashing.
        assert_eq!(
            w.check(Some(s), |_| panic!("not hashed again")),
            Verdict::Unchanged
        );
    }

    #[test]
    fn a_failed_restart_is_retried_on_the_next_poll() {
        let mut w = Watcher::new("aa-1".into(), true);
        let s = sig("/b", 1, 10, 1);
        for _ in 0..2 {
            assert!(matches!(
                w.check(Some(s.clone()), |_| Some("bb-2".into())),
                Verdict::Upgrade { .. }
            ));
        }
    }

    #[test]
    fn signature_of_follows_a_symlink_and_changes_with_the_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"one").expect("a");
        std::fs::write(&b, b"two!").expect("b");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&a, &link).expect("link");
        let before = signature_of(&link).expect("resolves");
        assert_eq!(before.path, a.canonicalize().unwrap());
        std::fs::remove_file(&link).expect("unlink");
        std::os::unix::fs::symlink(&b, &link).expect("relink");
        let after = signature_of(&link).expect("resolves");
        assert_ne!(before, after);
        std::fs::remove_file(&link).expect("unlink");
        std::os::unix::fs::symlink(dir.path().join("none"), &link).expect("dangling");
        assert_eq!(signature_of(&link), None);
        assert_eq!(signature_of(dir.path()), None, "a directory is no binary");
    }

    #[test]
    fn pick_existing_prefers_the_source_then_the_fallbacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source");
        let fallback = dir.path().join("fallback");
        std::fs::write(&source, b"").expect("source");
        std::fs::write(&fallback, b"").expect("fallback");
        let fallbacks = vec![dir.path().join("missing"), fallback.clone()];
        assert_eq!(pick_existing(Some(&source), &fallbacks), Some(source));
        assert_eq!(
            pick_existing(Some(&dir.path().join("gone")), &fallbacks),
            Some(fallback.clone())
        );
        assert_eq!(pick_existing(None, &fallbacks), Some(fallback));
        assert_eq!(pick_existing(None, &[dir.path().join("none")]), None);
    }

    #[test]
    fn a_bundle_exe_maps_to_its_recorded_source_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contents = dir.path().join("turbofig.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).expect("dirs");
        let source = dir.path().join("brew/bin/turbofig");
        std::fs::create_dir_all(source.parent().unwrap()).expect("brew dir");
        std::fs::write(&source, b"x").expect("source");
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<key>TurbofigSourcePath</key><string>{}</string>",
                source.display()
            ),
        )
        .expect("plist");
        let exe = contents.join("MacOS/turbofig");
        assert_eq!(source_binary_for_bundle_exe(&exe), Some(source));
        // Not a bundle: no source mapping.
        assert_eq!(
            source_binary_for_bundle_exe(&dir.path().join("brew/bin/turbofig")),
            None
        );
    }

    #[test]
    fn the_app_relaunches_only_when_the_disk_build_id_differs() {
        assert!(app_should_relaunch(Some("aa-1"), Some("bb-2")));
        assert!(!app_should_relaunch(Some("aa-1"), Some("aa-1")));
        assert!(!app_should_relaunch(None, Some("bb-2")));
        assert!(!app_should_relaunch(Some("aa-1"), None));
        assert!(!app_should_relaunch(None, None));
    }

    #[test]
    fn the_bare_command_waits_longer_than_the_app_quit_deadline() {
        let app = Duration::from_secs(10);
        assert!(app_quit_wait(app) > app);
        assert_eq!(app_quit_wait(app), Duration::from_secs(13));
    }

    #[test]
    fn a_refresh_that_quit_a_running_app_opens_a_new_instance() {
        assert_eq!(open_mode_after_refresh(true, true), OpenMode::NewInstance);
        assert_eq!(open_mode_after_refresh(true, false), OpenMode::Plain);
        assert_eq!(open_mode_after_refresh(false, true), OpenMode::Plain);
        assert_eq!(open_mode_after_refresh(false, false), OpenMode::Plain);
    }

    #[test]
    fn the_headless_service_owns_only_the_default_folder_daemon() {
        assert!(service_owns_daemon(true, ""));
        assert!(!service_owns_daemon(true, "/tmp/custom"));
        assert!(!service_owns_daemon(false, ""));
    }

    #[test]
    fn a_daemon_without_a_version_or_with_an_older_one_needs_a_restart() {
        assert!(daemon_needs_restart(None, "0.2.1"));
        assert!(daemon_needs_restart(Some("0.2.0"), "0.2.1"));
        assert!(!daemon_needs_restart(Some("0.2.1"), "0.2.1"));
        assert!(!daemon_needs_restart(Some("0.3.0"), "0.2.1"));
        assert!(!daemon_needs_restart(Some("garbage"), "0.2.1"));
    }

    #[test]
    fn the_drain_cap_is_60_s_or_the_request_timeout_plus_a_margin() {
        assert_eq!(
            drain_cap(Duration::from_secs(30)),
            Duration::from_secs(60),
            "the default timeout keeps the 60 s cap"
        );
        assert_eq!(drain_cap(Duration::from_secs(45)), Duration::from_secs(60));
        assert_eq!(drain_cap(Duration::from_secs(46)), Duration::from_secs(61));
        // A 600 s request timeout: a running job is never cut off at 60 s.
        assert_eq!(
            drain_cap(Duration::from_secs(600)),
            Duration::from_secs(615)
        );
    }

    #[test]
    fn a_caller_waits_at_least_as_long_as_the_daemons_drain() {
        let base = Duration::from_secs(75);
        assert_eq!(restart_wait(base, None), base);
        assert_eq!(restart_wait(base, Some(Duration::from_secs(30))), base);
        let long = restart_wait(base, Some(Duration::from_secs(600)));
        assert!(long > drain_cap(Duration::from_secs(600)));
    }

    #[test]
    fn a_build_that_keeps_failing_is_skipped_until_its_signature_changes() {
        let bad = sig("/b/turbofig", 7, 100, 50);
        let mut w = Watcher::new("aa-1".into(), true);
        let mut hashes = 0;
        for attempt in 1..=MAX_RESTART_ATTEMPTS {
            let verdict = w.check(Some(bad.clone()), |_| {
                hashes += 1;
                Some("bb-2".into())
            });
            assert!(matches!(verdict, Verdict::Upgrade { .. }), "{attempt}");
            let last = w.record_failure(&bad);
            assert_eq!(last, attempt == MAX_RESTART_ATTEMPTS);
        }
        assert_eq!(hashes, MAX_RESTART_ATTEMPTS);
        // Skipped now: no hash, no verdict to act on, poll after poll.
        for _ in 0..5 {
            let verdict = w.check(Some(bad.clone()), |_| {
                panic!("must not hash a failed build")
            });
            assert_eq!(verdict, Verdict::Unchanged);
        }
        // A new file at the same path (new inode) is looked at again.
        let fixed = sig("/b/turbofig", 8, 100, 90);
        let verdict = w.check(Some(fixed), |_| Some("cc-3".into()));
        assert!(matches!(verdict, Verdict::Upgrade { .. }));
    }

    #[test]
    fn failures_of_one_signature_do_not_count_against_another() {
        let a = sig("/b/turbofig", 7, 100, 50);
        let b = sig("/b/turbofig", 8, 100, 90);
        let mut w = Watcher::new("aa-1".into(), true);
        assert!(!w.record_failure(&a));
        assert!(!w.record_failure(&a));
        assert!(!w.record_failure(&b), "the count starts again for b");
    }

    #[test]
    fn sigpipe_is_ignored_again_after_a_failed_exec() {
        // `Command::exec` leaves SIGPIPE at the default on failure.
        // SAFETY: test-only signal disposition changes, restored below.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        restore_sigpipe_ignore();
        // SAFETY: `signal` returns the previous disposition.
        let was = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        assert_eq!(was, libc::SIG_IGN);
    }

    fn info(version: Option<&str>, build: Option<&str>) -> crate::secure_fs::LockInfo {
        crate::secure_fs::LockInfo {
            pid: Some(1),
            request_timeout: None,
            locked: true,
            version: version.map(str::to_owned),
            build_id: build.map(str::to_owned),
        }
    }

    #[test]
    fn a_restart_took_effect_when_the_build_changed_or_the_version_is_no_longer_older() {
        let asked = Some("aa-1");
        // Same daemon still: nothing happened yet.
        assert!(!restart_took_effect(
            asked,
            Some(&info(Some("0.1.9"), Some("aa-1"))),
            "0.2.0"
        ));
        // The new image rewrote the lock info.
        assert!(restart_took_effect(
            asked,
            Some(&info(Some("0.1.9"), Some("bb-2"))),
            "0.2.0"
        ));
        assert!(restart_took_effect(
            asked,
            Some(&info(Some("0.2.0"), Some("aa-1"))),
            "0.2.0"
        ));
        // A newer daemon is not "older" either.
        assert!(restart_took_effect(
            None,
            Some(&info(Some("0.3.0"), None)),
            "0.2.0"
        ));
    }

    #[test]
    fn a_restart_never_took_effect_on_missing_or_unreadable_facts() {
        assert!(!restart_took_effect("aa-1".into(), None, "0.2.0"));
        // No version, an unparseable version: "cannot tell" is not "done".
        assert!(!restart_took_effect(None, Some(&info(None, None)), "0.2.0"));
        assert!(!restart_took_effect(
            None,
            Some(&info(Some("garbage"), None)),
            "0.2.0"
        ));
        // An unknown build id on either side is not a change.
        assert!(!restart_took_effect(
            None,
            Some(&info(Some("0.1.9"), Some("bb-2"))),
            "0.2.0"
        ));
    }
}
