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

/// The one-line stdout JSON when no result arrived.
fn no_result_line(wait: Duration, outbox_file: &Path) -> String {
    json!({
        "ok": false,
        "code": "no_result",
        "error": format!("no result in {} s; the job may still be running", wait.as_secs()),
        "outbox": absolute(outbox_file),
    })
    .to_string()
}

/// The one-line stdout JSON when the daemon went away during the job.
fn daemon_stopped_line(outbox_file: &Path) -> String {
    let bin = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "turbofig".to_owned());
    json!({
        "ok": false,
        "code": "daemon_stopped",
        "error": format!("the turbofig daemon stopped during the job; run {bin} status, then retry"),
        "outbox": absolute(outbox_file),
    })
    .to_string()
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
    let fail = |msg: String| -> (Vec<u8>, i32) {
        eprintln!("turbofig js: {msg}");
        (Vec::new(), 1)
    };
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
    match wait_for_result(&result_path, wait, alive) {
        Ok(Waited::Result(mut out)) => {
            let _ = std::fs::remove_file(&result_path);
            if out.last() != Some(&b'\n') {
                out.push(b'\n');
            }
            (out, 0)
        }
        Ok(Waited::Timeout) => {
            let mut line = no_result_line(wait, &result_path).into_bytes();
            line.push(b'\n');
            (line, 1)
        }
        Ok(Waited::DaemonStopped) => {
            let mut line = daemon_stopped_line(&result_path).into_bytes();
            line.push(b'\n');
            (line, 1)
        }
        Err(e) => fail(format!("could not read the result: {e}")),
    }
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
    let second = match target {
        None => no_node_id_line().into_bytes(),
        Some(id) => {
            let mut shot_job = json!({"op": "screenshot", "nodeId": id});
            if let Some(key) = job.get("fileKey") {
                shot_job["fileKey"] = key.clone();
            }
            let (line, c) = run_job(dir, &shot_job, wait, alive);
            if c != 0 && line.is_empty() {
                json!({"ok": false, "code": "shot_failed", "error": "could not run the screenshot job; see stderr"})
                    .to_string()
                    .into_bytes()
            } else {
                line
            }
        }
    };
    out.extend_from_slice(&second);
    if out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    (out, 0)
}

/// How long to wait for a freshly started daemon to hold the lock and make
/// `inbox/` and `outbox/`.
const START_DEADLINE: Duration = Duration::from_secs(15);

/// Makes sure a daemon holds the lock on `dir`. When nothing holds it, starts
/// the daemon through the same detached path `turbofig mcp` uses, then waits
/// (bounded) until the lock is held and the bridge folders pass the daemon's
/// own checks. Uses no network connection.
fn ensure_daemon(dir: &Path) -> Result<(), String> {
    // Same folder checks as the daemon, before anything starts.
    crate::secure_fs::check_ancestors(dir).map_err(|e| e.to_string())?;
    crate::secure_fs::check_existing(dir).map_err(|e| e.to_string())?;
    if crate::secure_fs::bridge_lock_held(dir) {
        return Ok(());
    }
    let bin =
        std::env::current_exe().map_err(|e| format!("could not find the turbofig binary: {e}"))?;
    crate::spawn::spawn_detached_daemon(&bin, dir)
        .map_err(|e| format!("could not start the daemon: {e}"))?;
    let until = Instant::now() + START_DEADLINE;
    loop {
        if crate::secure_fs::bridge_lock_held(dir) {
            match checked_dirs(dir) {
                Ok(_) => return Ok(()),
                Err(e) if crate::secure_fs::is_refusal(&e) => return Err(e.to_string()),
                Err(_) => {}
            }
        }
        if Instant::now() >= until {
            return Err(format!(
                "no daemon took the lock in {}. An older turbofig daemon may be running: run `turbofig stop`, then retry. Log: {}",
                dir.display(),
                dir.join("daemon.log").display()
            ));
        }
        std::thread::sleep(SLOW_POLL);
    }
}

/// Resolves the bridge folder: `--dir` wins over `TURBOFIG_BRIDGE_DIR` and the
/// default. A trailing separator is stripped (it would make `lstat` follow a
/// symlink). The choice is exported as `TURBOFIG_BRIDGE_DIR` so a daemon this
/// command starts uses the same folder.
fn resolve_dir(dir_arg: Option<&str>) -> Result<PathBuf, String> {
    let Some(raw) = dir_arg else {
        return Ok(crate::config::bridge_dir_from_env());
    };
    let trimmed = raw.trim_end_matches('/');
    let val = if trimmed.is_empty() { raw } else { trimmed };
    if val.is_empty() {
        return Err("--dir is empty".to_owned());
    }
    std::env::set_var("TURBOFIG_BRIDGE_DIR", val);
    Ok(PathBuf::from(val))
}

/// Runs `turbofig js`. Returns the process exit code.
pub fn run(file_key: Option<&str>, shot: bool, dir_arg: Option<&str>) -> i32 {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("{USAGE}");
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
    let dir = match resolve_dir(dir_arg) {
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
    if let Err(e) = ensure_daemon(&dir) {
        eprintln!("turbofig js: {e}");
        return 1;
    }
    let wait = crate::config::request_timeout_from_env() + WAIT_MARGIN;
    let mut alive = || crate::secure_fs::bridge_lock_held(&dir);
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

    #[test]
    fn run_job_reports_no_result_with_the_outbox_path() {
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
        assert_eq!(v["code"], json!("no_result"));
        assert_eq!(
            v["error"],
            json!("no result in 0 s; the job may still be running")
        );
        assert!(v["outbox"].as_str().unwrap().contains("/bridge/outbox/"));
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
        assert_eq!(v["code"], json!("daemon_stopped"));
        assert!(v["error"].as_str().unwrap().ends_with("status, then retry"));
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
