//! `turbofig js`: run one Figma job through the file bridge and print the
//! result. It does the plumbing an agent used to type as a shell recipe:
//! read stdin, build the job, write it to `inbox/` atomically, wait for
//! `outbox/<id>.json`, print it. It runs before any async runtime, logging
//! or menu bar start-up, and it never reads or prints the pairing token (the
//! file bridge does not need it).

use serde_json::{json, Value};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Extra wait on top of the daemon's own job deadline, so the daemon's
/// timeout result arrives before this command gives up.
const WAIT_MARGIN: Duration = Duration::from_secs(5);

/// Poll every 5 ms for the first 200 ms, then every 20 ms.
const FAST_POLL: Duration = Duration::from_millis(5);
const FAST_POLL_WINDOW: Duration = Duration::from_millis(200);
const SLOW_POLL: Duration = Duration::from_millis(20);

/// Largest result file this command reads (a sanity bound, not a daemon limit).
const MAX_RESULT_BYTES: u64 = 256 * 1024 * 1024;

const USAGE: &str = "usage: turbofig js [--file <fileKey>] <<'EOF'\n<JS code, or a job JSON object with an \"op\" field>\nEOF";

/// Builds the job from stdin text. A JSON object with a string `op` field is
/// a ready job and is sent as given (`fileKey` is added when `--file` is set
/// and the job has none). Anything else is JS code for an `execute` job. A
/// JSON object with `op` is never valid as a JS statement, so the 2 cases do
/// not overlap.
pub fn build_job(stdin: &str, file_key: Option<&str>) -> Value {
    let trimmed = stdin.trim();
    if trimmed.starts_with('{') {
        if let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(trimmed) {
            if map.get("op").is_some_and(Value::is_string) {
                if let Some(key) = file_key {
                    map.entry("fileKey").or_insert_with(|| json!(key));
                }
                return Value::Object(map);
            }
        }
    }
    let mut job = json!({"op": "execute", "code": stdin});
    if let Some(key) = file_key {
        job["fileKey"] = json!(key);
    }
    job
}

/// A 32-hex-char job id from the OS CSPRNG.
fn new_job_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG (getrandom) must be available");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Applies the daemon's own folder checks (no symlink, owned by this user,
/// safe ancestors) to the bridge folder, `inbox/` and `outbox/`. Returns
/// `(inbox, outbox)`. A missing folder is an error: only the daemon makes them.
fn checked_dirs(dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    crate::secure_fs::check_ancestors(dir)?;
    crate::secure_fs::check_not_shared_writable(dir)?;
    let inbox = dir.join("inbox");
    let outbox = dir.join("outbox");
    for p in [dir, inbox.as_path(), outbox.as_path()] {
        match crate::secure_fs::check_existing(p)? {
            Some(meta) if meta.is_dir() => {}
            _ => {
                return Err(std::io::Error::other(format!(
                    "{} is missing or not a folder",
                    p.display()
                )))
            }
        }
    }
    Ok((inbox, outbox))
}

/// Writes the job atomically: `<id>.json.tmp` (O_EXCL, O_NOFOLLOW, 0600),
/// then rename to `<id>.json`. The inbox watcher only reads names that end
/// in `.json`, so it ignores the `.tmp` name.
fn write_job(inbox: &Path, id: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = inbox.join(format!("{id}.json.tmp"));
    let fin = inbox.join(format!("{id}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.flush());
    drop(file);
    let outcome = written.and_then(|()| std::fs::rename(&tmp, &fin));
    if outcome.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    outcome
}

/// Reads a result file with O_NOFOLLOW and O_NONBLOCK from one handle.
/// `Ok(None)` means "not there yet or not complete yet". The daemon writes a
/// result as `.tmp` then renames, so a file at the final name is complete;
/// the non-empty and parse checks only guard a foreign writer.
fn read_result(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("result is not a regular file"));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(std::io::Error::other("result is owned by another user"));
    }
    if meta.len() == 0 {
        return Ok(None);
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    (&mut file).take(MAX_RESULT_BYTES).read_to_end(&mut buf)?;
    if buf.is_empty() || serde_json::from_slice::<serde::de::IgnoredAny>(&buf).is_err() {
        return Ok(None);
    }
    Ok(Some(buf))
}

/// How the wait for a result ended.
enum Waited {
    Result(Vec<u8>),
    Timeout,
    /// `alive` said "no daemon" on 2 checks in a row.
    DaemonStopped,
}

/// How often the daemon lock is re-checked while polling.
const LIVENESS_EVERY: Duration = Duration::from_millis(500);

/// Polls for the result until `wait` passes. Every 500 ms it calls `alive`
/// (a network-free check); 2 failed checks in a row end the wait early. The
/// result is always tried first, so a result that landed before the daemon
/// went away is never lost.
fn wait_for_result(
    path: &Path,
    wait: Duration,
    alive: &mut dyn FnMut() -> bool,
) -> std::io::Result<Waited> {
    let start = Instant::now();
    let mut last_check = start;
    let mut misses = 0u32;
    loop {
        if let Some(bytes) = read_result(path)? {
            return Ok(Waited::Result(bytes));
        }
        let elapsed = start.elapsed();
        if elapsed >= wait {
            return Ok(Waited::Timeout);
        }
        if last_check.elapsed() >= LIVENESS_EVERY {
            last_check = Instant::now();
            if alive() {
                misses = 0;
            } else {
                misses += 1;
                if misses >= 2 {
                    return Ok(match read_result(path)? {
                        Some(bytes) => Waited::Result(bytes),
                        None => Waited::DaemonStopped,
                    });
                }
            }
        }
        std::thread::sleep(if elapsed < FAST_POLL_WINDOW {
            FAST_POLL
        } else {
            SLOW_POLL
        });
    }
}

/// Why a job got no result.
#[derive(Clone, Copy)]
enum NoResult {
    Timeout,
    DaemonStopped,
}

impl NoResult {
    fn reason(self) -> &'static str {
        match self {
            NoResult::Timeout => "timeout",
            NoResult::DaemonStopped => "daemon_stopped",
        }
    }
}

/// The one-line stdout JSON for a job that got no result. The caller already
/// tried to take the job back out of the inbox: `removed` says it worked, so
/// the job never ran (`not_started`: resend). When the inbox file was already
/// gone, the daemon claimed it and it may have run (`started_unknown`: check
/// before resending).
fn no_result_line(why: NoResult, wait: Duration, removed: bool, outbox_file: &Path) -> String {
    let bin = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "turbofig".to_owned());
    let cause = match why {
        NoResult::Timeout => format!("no result in {} s", wait.as_secs()),
        NoResult::DaemonStopped => {
            format!("the turbofig daemon stopped during the job (run {bin} status)")
        }
    };
    if removed {
        json!({
            "ok": false,
            "code": "not_started",
            "reason": why.reason(),
            "error": format!("{cause}; the job never ran; resend it"),
        })
        .to_string()
    } else {
        json!({
            "ok": false,
            "code": "started_unknown",
            "reason": why.reason(),
            "error": format!("{cause}; the job was claimed and it may have run; check before resending"),
            "outbox": absolute(outbox_file),
        })
        .to_string()
    }
}

/// The one-line stdout JSON for a failure of the bridge itself (an unusable
/// folder, a write failure, an unreadable result). Every exit 1 prints one.
fn bridge_error_line(msg: &str) -> String {
    json!({"ok": false, "code": "bridge_error", "error": msg}).to_string()
}

fn absolute(outbox_file: &Path) -> String {
    let abs = if outbox_file.is_absolute() {
        outbox_file.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(outbox_file))
            .unwrap_or_else(|_| outbox_file.to_path_buf())
    };
    abs.display().to_string()
}

/// Sends `job` through the bridge in `dir` and returns what to print and the
/// exit code. The result file is deleted after a successful read: the id is
/// unique, so nobody reads it again, and the daemon's 24 h prune is only a
/// backstop for results nobody read.
fn run_job(
    dir: &Path,
    job: &Value,
    wait: Duration,
    alive: &mut dyn FnMut() -> bool,
) -> (Vec<u8>, i32) {
    let line = |s: String| -> Vec<u8> {
        let mut b = s.into_bytes();
        b.push(b'\n');
        b
    };
    let fail = |msg: String| -> (Vec<u8>, i32) { (line(bridge_error_line(&msg)), 1) };
    let (inbox, outbox) = match checked_dirs(dir) {
        Ok(v) => v,
        Err(e) => return fail(e.to_string()),
    };
    let id = new_job_id();
    let bytes = job.to_string().into_bytes();
    if let Err(e) = write_job(&inbox, &id, &bytes) {
        return fail(format!("could not write the job: {e}"));
    }
    let result_path = outbox.join(format!("{id}.json"));
    let why = match wait_for_result(&result_path, wait, alive) {
        Ok(Waited::Result(mut out)) => {
            let _ = std::fs::remove_file(&result_path);
            if out.last() != Some(&b'\n') {
                out.push(b'\n');
            }
            return (out, 0);
        }
        Ok(Waited::Timeout) => NoResult::Timeout,
        Ok(Waited::DaemonStopped) => NoResult::DaemonStopped,
        Err(e) => return fail(format!("could not read the result: {e}")),
    };
    // Take the job back out of the inbox, so a daemon that starts later never
    // runs it a second time next to the caller's retry. The daemon claims a
    // job by removing the same file and skips an entry that is already gone
    // (NotFound), so exactly one side wins.
    let removed = std::fs::remove_file(inbox.join(format!("{id}.json"))).is_ok();
    (line(no_result_line(why, wait, removed, &result_path)), 1)
}

/// True for a Figma node id such as `12:34` or an instance id such as
/// `I12:34;56:78`.
fn looks_like_node_id(s: &str) -> bool {
    let body = s.strip_prefix('I').unwrap_or(s);
    !body.is_empty()
        && body.split(';').all(|part| {
            part.split_once(':').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
            })
        })
}

/// The id of one value: the value itself when it is a node id string, or its
/// `id` field when it is an object holding one.
fn id_of(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) if looks_like_node_id(s) => Some(s),
        Value::Object(m) => m
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| looks_like_node_id(s)),
        _ => None,
    }
}

/// The first node id in a returned value: the value itself, else its `id`,
/// else the first array element that is an id or holds an `id`.
pub fn find_node_id(v: &Value) -> Option<String> {
    if let Some(id) = id_of(v) {
        return Some(id.to_owned());
    }
    v.as_array()?.iter().find_map(id_of).map(str::to_owned)
}

/// What `--shot` needs from the first line of output: `Some(Some(id))` for
/// an ok result with a node id, `Some(None)` for an ok result without one,
/// `None` when the first job failed (no screenshot, no second line).
fn shot_target(first_line: &[u8]) -> Option<Option<String>> {
    let v: Value = serde_json::from_slice(first_line).ok()?;
    if v.get("ok") != Some(&Value::Bool(true)) {
        return None;
    }
    Some(v.get("result").and_then(find_node_id))
}

fn no_node_id_line() -> String {
    json!({
        "ok": false,
        "code": "no_node_id",
        "error": "--shot found no node id in the result; return the node id",
    })
    .to_string()
}

/// Builds the job, sends it, and with `shot` sends a screenshot job for the
/// first node id in an ok result. Returns the stdout bytes and the exit code.
/// Exit 2: usage error (a job that is too large, or `--shot` with a job that
/// is not `execute`); nothing is written to the inbox then.
fn execute(
    dir: &Path,
    text: &str,
    file_key: Option<&str>,
    shot: bool,
    wait: Duration,
    alive: &mut dyn FnMut() -> bool,
) -> (Vec<u8>, i32) {
    let job = build_job(text, file_key);
    if shot && job["op"] != json!("execute") {
        eprintln!("turbofig js: --shot works only with JS code or an execute job");
        return (Vec::new(), 2);
    }
    let cap = crate::bridge::MAX_JOB_BYTES;
    if job.to_string().len() as u64 > cap {
        eprintln!(
            "turbofig js: the job is larger than {} MiB",
            cap / (1024 * 1024)
        );
        return (Vec::new(), 2);
    }
    let (mut out, code) = run_job(dir, &job, wait, alive);
    if !shot || code != 0 {
        return (out, code);
    }
    let first = out.clone();
    let Some(target) = shot_target(&first) else {
        return (out, 0);
    };
    // Exit rule: 0 when the first result printed and the screenshot job got a
    // result (even an `ok:false` one from the plugin, or `no_node_id`); 1 when
    // the screenshot job got no result (`not_started`, `started_unknown`,
    // `bridge_error`).
    let mut shot_code = 0;
    let second = match target {
        None => no_node_id_line().into_bytes(),
        Some(id) => {
            let mut shot_job = json!({"op": "screenshot", "nodeId": id});
            if let Some(key) = job.get("fileKey") {
                shot_job["fileKey"] = key.clone();
            }
            let (line, c) = run_job(dir, &shot_job, wait, alive);
            shot_code = c;
            line
        }
    };
    out.extend_from_slice(&second);
    if out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    (out, shot_code)
}

/// How long to wait for a freshly started daemon to hold the lock and make
/// `inbox/` and `outbox/`.
const START_DEADLINE: Duration = Duration::from_secs(15);

/// How long to wait for launchd's own (re)launch of the autostart service.
/// Same value as `proxy.rs`'s `SUPERVISED_RELAUNCH_WAIT`.
const SUPERVISED_RELAUNCH_WAIT: Duration = Duration::from_secs(30);

/// Wait used when the daemon's recorded request timeout cannot be read. The
/// lock check still ends the wait early when the daemon dies.
const FALLBACK_WAIT: Duration = Duration::from_secs(600);

/// How the daemon that serves the bridge folder can be watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A daemon holds `daemon.lock`: liveness is a lock query.
    Locked,
    /// A daemon answers on the port but holds no lock (an older version, or a
    /// filesystem without locks): only the plain timeout applies.
    Legacy,
}

/// True when something accepts a TCP connect on the daemon's MCP port and
/// the bridge folders exist. This is only the fallback for a daemon that
/// holds no lock; it sends no bytes.
fn legacy_daemon_alive(dir: &Path) -> bool {
    if checked_dirs(dir).is_err() {
        return false;
    }
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], crate::config::port_from_env()));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

/// Same meaning as `turbofig mcp`'s handling of a launchd-supervised daemon
/// (`proxy.rs`: `must_spawn_after_restart`, `SUPERVISED_RELAUNCH_WAIT`):
/// do not race launchd's relaunch with an unsupervised spawn. True when the
/// headless autostart plist exists and the bridge folder is the default one
/// (the plist's daemon serves that folder).
fn should_wait_for_launchd(plist_exists: bool, default_folder: bool) -> bool {
    plist_exists && default_folder
}

fn headless_plist_exists(agents_dir: &Path) -> bool {
    agents_dir.join(crate::launchd::plist_file_name()).exists()
}

/// Asks launchd to start the headless service if it is not running
/// (`launchctl kickstart`, no `-k`: a running service is left alone). Best
/// effort; macOS only.
fn kickstart_headless_service() {
    #[cfg(target_os = "macos")]
    if let Ok(uid) = crate::launchd::current_uid() {
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["kickstart", &crate::launchd::service_target(&uid)])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Polls until a daemon serves `dir` (the lock is held, or with
/// `allow_legacy` a daemon answers on the port) and the bridge folders pass
/// the daemon's own checks. `Ok(None)` is a timeout.
fn wait_for_daemon(
    dir: &Path,
    deadline: Duration,
    allow_legacy: bool,
) -> Result<Option<Mode>, String> {
    let until = Instant::now() + deadline;
    loop {
        if crate::secure_fs::bridge_lock_held(dir) {
            match checked_dirs(dir) {
                Ok(_) => return Ok(Some(Mode::Locked)),
                Err(e) if crate::secure_fs::is_refusal(&e) => return Err(e.to_string()),
                Err(_) => {}
            }
        } else if allow_legacy && legacy_daemon_alive(dir) {
            return Ok(Some(Mode::Legacy));
        }
        if Instant::now() >= until {
            return Ok(None);
        }
        std::thread::sleep(SLOW_POLL);
    }
}

/// Makes sure a daemon serves `dir` and says how to watch it. Uses no network
/// connection, except one plain TCP connect to the daemon port when no lock
/// is held (the older-daemon check, and the start wait on a filesystem
/// without locks).
///
/// Order: a lock holder means a daemon is up. Else a daemon answering on the
/// port with the folders present is an older daemon (`Mode::Legacy`). Else,
/// with the default folder and an autostart plist, launchd is asked to start
/// the supervised service and given `SUPERVISED_RELAUNCH_WAIT` before this
/// process spawns an unsupervised one (an unsupervised daemon makes the
/// later launchd start exit "already running" and leaves the service
/// stopped). Last, the daemon is spawned through the same detached path
/// `turbofig mcp` uses.
fn ensure_daemon(dir: &Path, default_folder: bool) -> Result<Mode, String> {
    // Same folder checks as the daemon, before anything starts.
    crate::secure_fs::check_ancestors(dir).map_err(|e| e.to_string())?;
    crate::secure_fs::check_existing(dir).map_err(|e| e.to_string())?;
    crate::secure_fs::check_not_shared_writable(dir).map_err(|e| e.to_string())?;
    if crate::secure_fs::bridge_lock_held(dir) {
        return Ok(Mode::Locked);
    }
    if legacy_daemon_alive(dir) {
        return Ok(Mode::Legacy);
    }
    let plist = headless_plist_exists(&crate::launchd::launch_agents_dir_from_env());
    if should_wait_for_launchd(plist, default_folder) {
        kickstart_headless_service();
        if let Some(mode) = wait_for_daemon(dir, SUPERVISED_RELAUNCH_WAIT, false)? {
            return Ok(mode);
        }
    }
    let bin =
        std::env::current_exe().map_err(|e| format!("could not find the turbofig binary: {e}"))?;
    crate::spawn::spawn_detached_daemon(&bin, dir)
        .map_err(|e| format!("could not start the daemon: {e}"))?;
    match wait_for_daemon(dir, START_DEADLINE, true)? {
        Some(mode) => Ok(mode),
        None => Err(format!(
            "no daemon took over {} within {} s; see {}",
            dir.display(),
            START_DEADLINE.as_secs(),
            dir.join("daemon.log").display()
        )),
    }
}

/// Resolves the bridge folder to an absolute path: `--dir` wins over
/// `TURBOFIG_BRIDGE_DIR` and the default. A relative path would break the
/// daemon this command starts (it runs with the bridge folder as its working
/// directory). A trailing separator is stripped (it would make `lstat`
/// follow a symlink). The choice is exported as `TURBOFIG_BRIDGE_DIR` so a
/// started daemon uses the same folder, except when it is the default
/// `$HOME/.turbofig`: a daemon started with the variable set treats the
/// folder as custom (no chmod, no outbox marker), so the variable is left
/// unset then. Returns the folder and whether it is the default one.
fn resolve_dir(dir_arg: Option<&str>) -> Result<(PathBuf, bool), String> {
    let raw: PathBuf = match dir_arg {
        Some(raw) => {
            let trimmed = raw.trim_end_matches('/');
            let val = if trimmed.is_empty() { raw } else { trimmed };
            if val.is_empty() {
                return Err("--dir is empty".to_owned());
            }
            PathBuf::from(val)
        }
        None => crate::config::bridge_dir_from_env(),
    };
    let abs = std::path::absolute(&raw).map_err(|e| format!("bad bridge folder: {e}"))?;
    let default = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| std::path::absolute(PathBuf::from(h).join(".turbofig")))
        .and_then(Result::ok);
    let is_default = default.as_deref() == Some(abs.as_path());
    if is_default {
        std::env::remove_var("TURBOFIG_BRIDGE_DIR");
    } else {
        std::env::set_var("TURBOFIG_BRIDGE_DIR", &abs);
    }
    Ok((abs, is_default))
}

/// How long to wait for the first byte on stdin.
const STDIN_FIRST_BYTE_WAIT: Duration = Duration::from_secs(10);

/// Waits until stdin is readable (data, or EOF/hangup) or `wait` passes, so a
/// pipe that never gets data and never closes cannot block forever. A regular
/// file or a closed pipe is ready at once.
fn stdin_ready(wait: Duration) -> bool {
    let mut fds = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let until = Instant::now() + wait;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let ms = left.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut fds, 1, ms) };
        if rc > 0 {
            return true;
        }
        if rc == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}

fn stdin_wait() -> Duration {
    // Test seam, like the other TURBOFIG_TEST_* variables.
    std::env::var("TURBOFIG_TEST_JS_STDIN_WAIT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(STDIN_FIRST_BYTE_WAIT)
}

/// Prints the one JSON line every exit 1 carries, then returns 1.
fn exit_with_bridge_error(msg: &str) -> i32 {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", bridge_error_line(msg));
    let _ = stdout.flush();
    1
}

/// Runs `turbofig js`. Returns the process exit code: 0 a result printed,
/// 1 no result (always with one JSON line on stdout: `not_started`,
/// `started_unknown` or `bridge_error`), 2 a usage error (message on stderr).
pub fn run(file_key: Option<&str>, shot: bool, dir_arg: Option<&str>) -> i32 {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("{USAGE}");
        return 2;
    }
    if !stdin_ready(stdin_wait()) {
        eprintln!("turbofig js: no input on stdin\n{USAGE}");
        return 2;
    }
    let mut raw = Vec::new();
    let cap = crate::bridge::MAX_JOB_BYTES;
    if let Err(e) = stdin.lock().take(cap + 1).read_to_end(&mut raw) {
        eprintln!("turbofig js: could not read stdin: {e}");
        return 2;
    }
    if raw.len() as u64 > cap {
        eprintln!(
            "turbofig js: input is larger than {} MiB",
            cap / (1024 * 1024)
        );
        return 2;
    }
    let Ok(text) = String::from_utf8(raw) else {
        eprintln!("turbofig js: stdin is not valid UTF-8");
        return 2;
    };
    if text.trim().is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    // Check usage errors before the daemon starts.
    let (dir, default_folder) = match resolve_dir(dir_arg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("turbofig js: {e}");
            return 2;
        }
    };
    let probe = build_job(&text, file_key);
    if shot && probe["op"] != json!("execute") {
        eprintln!("turbofig js: --shot works only with JS code or an execute job");
        return 2;
    }
    if probe.to_string().len() as u64 > cap {
        eprintln!(
            "turbofig js: the job is larger than {} MiB",
            cap / (1024 * 1024)
        );
        return 2;
    }
    drop(probe);
    let mode = match ensure_daemon(&dir, default_folder) {
        Ok(m) => m,
        Err(e) => return exit_with_bridge_error(&e),
    };
    // The wait follows the timeout the daemon recorded, not this process's
    // environment (an autostart service can carry a different value).
    let wait = match mode {
        Mode::Locked => crate::secure_fs::read_lock_request_timeout(&dir)
            .map(|t| t + WAIT_MARGIN)
            .unwrap_or(FALLBACK_WAIT),
        Mode::Legacy => crate::config::request_timeout_from_env() + WAIT_MARGIN,
    };
    let mut alive = || mode == Mode::Legacy || crate::secure_fs::bridge_lock_held(&dir);
    let (out, code) = execute(&dir, &text, file_key, shot, wait, &mut alive);
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_js_becomes_an_execute_job() {
        let job = build_job("return figma.root.name;\n", None);
        assert_eq!(
            job,
            json!({"op": "execute", "code": "return figma.root.name;\n"})
        );
    }

    #[test]
    fn a_job_object_with_op_is_sent_unchanged() {
        let job = build_job("  {\"op\":\"status\"}\n", None);
        assert_eq!(job, json!({"op": "status"}));
    }

    #[test]
    fn file_flag_adds_file_key_to_code_and_to_a_job_without_one() {
        assert_eq!(build_job("return 1", Some("abc"))["fileKey"], json!("abc"));
        assert_eq!(
            build_job("{\"op\":\"status\"}", Some("abc")),
            json!({"op": "status", "fileKey": "abc"})
        );
    }

    #[test]
    fn file_flag_never_overrides_an_existing_file_key() {
        let job = build_job("{\"op\":\"status\",\"fileKey\":\"mine\"}", Some("abc"));
        assert_eq!(job["fileKey"], json!("mine"));
    }

    #[test]
    fn json_without_a_string_op_or_broken_json_is_code() {
        for src in [
            "{\"op\":1}",
            "{\"a\":1}",
            "{ not json",
            "[1,2]",
            "{\"op\":\"x\"",
        ] {
            let job = build_job(src, None);
            assert_eq!(job["op"], json!("execute"), "{src}");
            assert_eq!(job["code"], json!(src), "{src}");
        }
    }

    #[test]
    fn a_js_block_that_starts_with_a_brace_is_code() {
        let src = "{ const a = 1; return a; }";
        assert_eq!(build_job(src, None)["op"], json!("execute"));
    }

    #[test]
    fn job_ids_are_32_hex_chars_and_differ() {
        let a = new_job_id();
        let b = new_job_id();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    fn bridge(tmp: &Path) -> PathBuf {
        let dir = tmp.join("bridge");
        crate::secure_fs::prepare_bridge_dirs(&dir, true).unwrap();
        dir
    }

    #[test]
    fn write_job_is_atomic_owner_only_and_leaves_no_tmp() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        write_job(&dir.join("inbox"), "abc", b"{\"op\":\"status\"}").unwrap();
        let p = dir.join("inbox/abc.json");
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"op\":\"status\"}");
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        assert!(!dir.join("inbox/abc.json.tmp").exists());
    }

    #[test]
    fn write_job_refuses_a_planted_symlink_at_the_tmp_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let target = tmp.path().join("target");
        std::os::unix::fs::symlink(&target, dir.join("inbox/abc.json.tmp")).unwrap();
        assert!(write_job(&dir.join("inbox"), "abc", b"x").is_err());
        assert!(!target.exists());
    }

    #[test]
    fn symlinked_inbox_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        std::fs::remove_dir(dir.join("inbox")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), dir.join("inbox")).unwrap();
        assert!(checked_dirs(&dir).is_err());
    }

    #[test]
    fn missing_inbox_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(checked_dirs(&tmp.path().join("nothing")).is_err());
    }

    #[test]
    fn read_result_ignores_empty_and_partial_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("r.json");
        assert_eq!(read_result(&p).unwrap(), None);
        std::fs::write(&p, b"").unwrap();
        assert_eq!(read_result(&p).unwrap(), None);
        std::fs::write(&p, b"{\"ok\":tr").unwrap();
        assert_eq!(read_result(&p).unwrap(), None);
        std::fs::write(&p, b"{\"ok\":true}").unwrap();
        assert_eq!(read_result(&p).unwrap(), Some(b"{\"ok\":true}".to_vec()));
    }

    #[test]
    fn read_result_refuses_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.json");
        std::fs::write(&real, b"{}").unwrap();
        let link = tmp.path().join("link.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(read_result(&link).is_err());
    }

    #[test]
    fn run_job_prints_the_result_and_deletes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let inbox = dir.join("inbox");
        let outbox = dir.join("outbox");
        // A fake daemon: answer the first job that appears in the inbox.
        let fake = std::thread::spawn({
            let outbox = outbox.clone();
            move || loop {
                for e in std::fs::read_dir(&inbox).unwrap().flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if let Some(id) = name.strip_suffix(".json") {
                        std::fs::write(outbox.join(format!("{id}.json")), b"{\"ok\":false}")
                            .unwrap();
                        return id.to_owned();
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let (out, code) = run_job(
            &dir,
            &json!({"op": "status"}),
            Duration::from_secs(5),
            &mut || true,
        );
        let id = fake.join().unwrap();
        assert_eq!(code, 0);
        assert_eq!(out, b"{\"ok\":false}\n");
        assert!(!outbox.join(format!("{id}.json")).exists());
    }

    // ── fake daemon for execute()/--shot tests ──────────────────────────────
    /// Answers `replies.len()` jobs in order; returns the jobs it received.
    fn fake_daemon(dir: &Path, replies: Vec<String>) -> std::thread::JoinHandle<Vec<Value>> {
        let inbox = dir.join("inbox");
        let outbox = dir.join("outbox");
        std::thread::spawn(move || {
            let mut got = Vec::new();
            for reply in replies {
                loop {
                    let hit = std::fs::read_dir(&inbox)
                        .unwrap()
                        .flatten()
                        .find(|e| e.file_name().to_string_lossy().ends_with(".json"));
                    if let Some(e) = hit {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let id = name.strip_suffix(".json").unwrap().to_owned();
                        let bytes = std::fs::read(e.path()).unwrap();
                        std::fs::remove_file(e.path()).unwrap();
                        got.push(serde_json::from_slice(&bytes).unwrap());
                        std::fs::write(outbox.join(format!("{id}.json")), reply).unwrap();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            got
        })
    }

    const WAIT: Duration = Duration::from_secs(5);

    fn inbox_count(dir: &Path) -> usize {
        std::fs::read_dir(dir.join("inbox")).unwrap().count()
    }

    #[test]
    fn a_timeout_with_the_job_still_unclaimed_removes_it_and_says_not_started() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let (out, code) = run_job(
            &dir,
            &json!({"op": "status"}),
            Duration::from_millis(50),
            &mut || true,
        );
        assert_eq!(code, 1);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["code"], json!("not_started"));
        assert_eq!(v["reason"], json!("timeout"));
        assert!(v["error"].as_str().unwrap().contains("resend"));
        assert_eq!(inbox_count(&dir), 0, "the job must be gone from the inbox");
    }

    #[test]
    fn a_timeout_after_the_daemon_claimed_the_job_says_started_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let inbox = dir.join("inbox");
        // A fake daemon that claims the job (removes the file) and never answers.
        let fake = std::thread::spawn(move || loop {
            if let Some(e) = std::fs::read_dir(&inbox)
                .unwrap()
                .flatten()
                .find(|e| e.file_name().to_string_lossy().ends_with(".json"))
            {
                std::fs::remove_file(e.path()).unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        });
        let (out, code) = run_job(
            &dir,
            &json!({"op": "status"}),
            Duration::from_millis(300),
            &mut || true,
        );
        fake.join().unwrap();
        assert_eq!(code, 1);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["code"], json!("started_unknown"));
        assert_eq!(v["reason"], json!("timeout"));
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("check before resending"));
        assert!(v["outbox"].as_str().unwrap().contains("/bridge/outbox/"));
    }

    #[test]
    fn every_exit_1_path_prints_one_json_line() {
        let tmp = tempfile::tempdir().unwrap();
        let (out, code) = run_job(
            &tmp.path().join("missing"),
            &json!({"op": "status"}),
            Duration::from_millis(50),
            &mut || true,
        );
        assert_eq!(code, 1);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["code"], json!("bridge_error"));
        assert_eq!(v["ok"], json!(false));
    }

    #[test]
    fn a_group_writable_bridge_folder_is_a_bridge_error() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        let (out, code) = run_job(
            &dir,
            &json!({"op": "status"}),
            Duration::from_millis(50),
            &mut || true,
        );
        assert_eq!(code, 1);
        assert!(String::from_utf8(out).unwrap().contains("bridge_error"));
        assert_eq!(inbox_count(&dir), 0);
    }

    #[test]
    fn the_launchd_decision_waits_only_for_a_plist_on_the_default_folder() {
        assert!(should_wait_for_launchd(true, true));
        assert!(!should_wait_for_launchd(true, false));
        assert!(!should_wait_for_launchd(false, true));
        assert!(!should_wait_for_launchd(false, false));
        let tmp = tempfile::tempdir().unwrap();
        assert!(!headless_plist_exists(tmp.path()));
        std::fs::write(tmp.path().join(crate::launchd::plist_file_name()), "x").unwrap();
        assert!(headless_plist_exists(tmp.path()));
    }

    #[test]
    fn stdin_that_never_gets_data_times_out() {
        // A pipe whose writer stays open and silent. Run poll on fd 0 of a
        // child is awkward in-process, so test the poll helper on a pipe.
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let mut p = libc::pollfd {
            fd: fds[0],
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut p, 1, 50) };
        assert_eq!(rc, 0);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn a_failed_screenshot_job_makes_the_exit_code_1_but_a_plugin_error_does_not() {
        // No node-id screenshot answer: the second job times out.
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let fake = fake_daemon(&dir, vec!["{\"ok\":true,\"result\":\"1:2\"}".into()]);
        let (out, c) = execute(
            &dir,
            "return '1:2';",
            None,
            true,
            Duration::from_millis(300),
            &mut || true,
        );
        fake.join().unwrap();
        assert_eq!(c, 1);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let v: Value = serde_json::from_str(lines[1]).unwrap();
        assert!(["not_started", "started_unknown"].contains(&v["code"].as_str().unwrap()));
        assert_eq!(inbox_count(&dir), 0);

        // The plugin answered the screenshot with ok:false: still exit 0.
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let fake = fake_daemon(
            &dir,
            vec![
                "{\"ok\":true,\"result\":\"1:2\"}".into(),
                "{\"ok\":false,\"code\":\"plugin_disconnected\",\"error\":\"x\"}".into(),
            ],
        );
        let (_, c) = execute(&dir, "return '1:2';", None, true, WAIT, &mut || true);
        fake.join().unwrap();
        assert_eq!(c, 0);
    }

    #[test]
    fn a_code_body_with_an_eof_line_arrives_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let code = "const a = `\nEOF\n`;\nEOF\n  EOF\nreturn a;\n";
        let fake = fake_daemon(&dir, vec!["{\"ok\":true}".into()]);
        let (_, c) = execute(&dir, code, None, false, WAIT, &mut || true);
        assert_eq!(c, 0);
        assert_eq!(fake.join().unwrap()[0]["code"], json!(code));
    }

    #[test]
    fn unicode_and_emoji_arrive_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let code = "return 'Zażółć gęślą jaźń \u{1F680} \u{65E5}\u{672C}\u{8A9E} \u{202E}';";
        let fake = fake_daemon(&dir, vec!["{\"ok\":true}".into()]);
        let (_, c) = execute(&dir, code, Some("k"), false, WAIT, &mut || true);
        assert_eq!(c, 0);
        let got = fake.join().unwrap();
        assert_eq!(got[0]["code"], json!(code));
        assert_eq!(got[0]["fileKey"], json!("k"));
    }

    #[test]
    fn a_bridge_path_with_a_space_works() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("my bridge dir");
        crate::secure_fs::prepare_bridge_dirs(&dir, true).unwrap();
        let fake = fake_daemon(&dir, vec!["{\"ok\":true}".into()]);
        let (out, c) = execute(&dir, "{\"op\":\"status\"}", None, false, WAIT, &mut || true);
        fake.join().unwrap();
        assert_eq!((out.as_slice(), c), (&b"{\"ok\":true}\n"[..], 0));
    }

    #[test]
    fn an_oversize_job_is_refused_and_nothing_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let code = "a".repeat(crate::bridge::MAX_JOB_BYTES as usize);
        let (out, c) = execute(&dir, &code, None, false, WAIT, &mut || true);
        assert_eq!(c, 2);
        assert!(out.is_empty());
        assert_eq!(std::fs::read_dir(dir.join("inbox")).unwrap().count(), 0);
    }

    #[test]
    fn find_node_id_covers_string_object_array_and_nothing() {
        assert_eq!(find_node_id(&json!("12:34")), Some("12:34".into()));
        assert_eq!(
            find_node_id(&json!("I12:34;56:78")),
            Some("I12:34;56:78".into())
        );
        assert_eq!(
            find_node_id(&json!({"id": "1:2", "name": "x"})),
            Some("1:2".into())
        );
        assert_eq!(
            find_node_id(&json!(["nope", {"id": "3:4"}, "5:6"])),
            Some("3:4".into())
        );
        assert_eq!(find_node_id(&json!(["nope", "5:6"])), Some("5:6".into()));
        for none in [
            json!("hello"),
            json!("12:"),
            json!(":34"),
            json!("1:2:3"),
            json!({"id": "x"}),
            json!({"name": "a"}),
            json!([]),
            json!(null),
            json!(12),
            json!("I"),
        ] {
            assert_eq!(find_node_id(&none), None, "{none}");
        }
    }

    #[test]
    fn shot_sends_a_screenshot_job_and_prints_two_lines() {
        for (result, id) in [
            ("\"12:34\"", "12:34"),
            ("{\"id\":\"7:8\"}", "7:8"),
            ("[\"x\",{\"id\":\"9:10\"}]", "9:10"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = bridge(tmp.path());
            let shot_reply = "{\"ok\":true,\"path\":\"/o/1.png\",\"w\":1,\"h\":2}";
            let fake = fake_daemon(
                &dir,
                vec![
                    format!("{{\"ok\":true,\"result\":{result}}}"),
                    shot_reply.into(),
                ],
            );
            let (out, c) = execute(&dir, "return x;", Some("fk"), true, WAIT, &mut || true);
            let got = fake.join().unwrap();
            assert_eq!(c, 0);
            assert_eq!(
                got[1],
                json!({"op": "screenshot", "nodeId": id, "fileKey": "fk"})
            );
            let text = String::from_utf8(out).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 2, "{text}");
            assert_eq!(lines[1], shot_reply);
        }
    }

    #[test]
    fn shot_without_a_node_id_prints_no_node_id() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let fake = fake_daemon(&dir, vec!["{\"ok\":true,\"result\":\"hello\"}".into()]);
        let (out, c) = execute(&dir, "return 'hello';", None, true, WAIT, &mut || true);
        assert_eq!(fake.join().unwrap().len(), 1);
        assert_eq!(c, 0);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["code"], json!("no_node_id"));
    }

    #[test]
    fn shot_after_a_failed_job_prints_only_the_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let reply = "{\"ok\":false,\"code\":\"plugin_disconnected\",\"error\":\"x\"}";
        let fake = fake_daemon(&dir, vec![reply.into()]);
        let (out, c) = execute(&dir, "return '1:2';", None, true, WAIT, &mut || true);
        fake.join().unwrap();
        assert_eq!(c, 0);
        assert_eq!(String::from_utf8(out).unwrap(), format!("{reply}\n"));
    }

    #[test]
    fn shot_with_a_non_execute_job_is_a_usage_error_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let (out, c) = execute(&dir, "{\"op\":\"status\"}", None, true, WAIT, &mut || true);
        assert_eq!(c, 2);
        assert!(out.is_empty());
        assert_eq!(std::fs::read_dir(dir.join("inbox")).unwrap().count(), 0);
    }

    #[test]
    fn a_dead_daemon_is_reported_after_2_failed_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let t = Instant::now();
        let (out, c) = execute(
            &dir,
            "return 1;",
            None,
            false,
            Duration::from_secs(30),
            &mut || false,
        );
        assert_eq!(c, 1);
        assert!(t.elapsed() < Duration::from_secs(3));
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["code"], json!("not_started"));
        assert_eq!(v["reason"], json!("daemon_stopped"));
        assert_eq!(inbox_count(&dir), 0);
    }

    #[test]
    fn one_failed_check_among_good_ones_does_not_stop_the_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bridge(tmp.path());
        let mut n = 0;
        let (_, c) = execute(
            &dir,
            "return 1;",
            None,
            false,
            Duration::from_millis(1800),
            &mut || {
                n += 1;
                n % 2 == 0
            },
        );
        // Checks alternate false/true, never 2 false in a row: a timeout.
        assert_eq!(c, 1);
    }
}
