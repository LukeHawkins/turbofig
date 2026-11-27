//! Client side of the daemon proof.
//!
//! Another local account can listen on the daemon port while the daemon is
//! down. A client that sends the pairing token to that listener leaks the
//! token. So every client verifies the daemon first: `GET /health` with a
//! fresh nonce and no `Authorization` header. The real daemon answers with
//! a proof header that only the token holder can compute (`daemon_auth`).
//! A client sends the token only after that check passes.
//!
//! This module also caps every read from the daemon, so a hostile listener
//! cannot exhaust memory with an endless body.

use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::daemon_auth::{new_nonce, verify, LABEL_HTTP_DAEMON, NONCE_HEADER, PROOF_HEADER};

/// Largest `/health` body a client reads: 64 KiB.
pub const HEALTH_BODY_CAP: usize = 64 * 1024;
/// Largest `/job` result a client reads: 32 MiB.
pub const RESULT_BODY_CAP: usize = 32 * 1024 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Why a daemon is not verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// Nothing accepted the connection.
    Unreachable,
    /// A listener answered, but it did not prove it holds the token. The
    /// owner is unknown, or is this Mac user (an older daemon, another app,
    /// or a replaced token file).
    Unverified { addr: String },
    /// Like `Unverified`, and `lsof` showed the listener belongs to another
    /// Mac user.
    OtherUser { addr: String, owner_uid: u32 },
    /// A listener answered, but there is no token file to check it against.
    NoToken { addr: String },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Unreachable => write!(f, "no turbofig daemon is reachable"),
            VerifyError::Unverified { addr } => write!(
                f,
                "a process that is not your turbofig daemon is listening on {addr}. {}",
                foreign_listener_advice(port_of(addr), None, 0)
            ),
            VerifyError::OtherUser { addr, .. } => write!(
                f,
                "a process that is not your turbofig daemon is listening on {addr}. {}",
                another_user_advice(port_of(addr))
            ),
            VerifyError::NoToken { addr } => write!(
                f,
                "a process is listening on {addr}, but there is no pairing token file to verify it against. \
                 A running daemon rewrites a deleted token file within a second, so retry. \
                 If it persists, quit turbofig from the menu bar and start it again."
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Error from a request that is sent only after the daemon is verified.
#[derive(Debug)]
pub enum ClientError {
    Verify(VerifyError),
    Http(reqwest::Error),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Verify(e) => e.fmt(f),
            ClientError::Http(e) => e.fmt(f),
        }
    }
}

/// The base URL of the daemon on `port`.
pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn addr_of(base_url: &str) -> String {
    base_url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned()
}

/// A client that never follows a redirect and never uses a proxy.
fn build_client(timeout: Option<Duration>) -> Result<reqwest::Client, reqwest::Error> {
    let mut b = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(crate::spawn::ADMIN_CLIENT_TIMEOUT);
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    b.build()
}

/// Reads at most `cap` bytes of the body. `None` when the body is larger.
pub async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Option<Vec<u8>> {
    if resp.content_length().is_some_and(|l| l > cap as u64) {
        return None;
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if buf.len() + chunk.len() > cap {
            return None;
        }
        buf.extend_from_slice(&chunk);
    }
    Some(buf)
}

/// Reads a JSON body of at most `cap` bytes.
pub async fn read_json_capped(resp: reqwest::Response, cap: usize) -> Option<serde_json::Value> {
    let bytes = read_capped(resp, cap).await?;
    serde_json::from_slice(&bytes).ok()
}

/// How a listener that answered `/health` is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    /// It proved it holds the token.
    Verified,
    /// No valid proof, but a process of the same user owns the socket. This
    /// is an older turbofig daemon (it has no proof support). Same-user
    /// processes are outside the threat model, so it is treated as before.
    LegacySameUser,
    /// Anything else: treat it as not the daemon.
    Foreign,
}

/// Classifies a listener. `listener_uid` is the owner of the listening
/// socket, when known. No owner means Foreign: another user's sockets are
/// invisible to a non-root `lsof`.
pub fn classify_listener(proof_ok: bool, listener_uid: Option<u32>, my_uid: u32) -> Listener {
    if proof_ok {
        Listener::Verified
    } else if listener_uid == Some(my_uid) {
        Listener::LegacySameUser
    } else {
        Listener::Foreign
    }
}

/// What to tell the user when `lsof` shows another Mac user owns the listener
/// on `port`: no command of this user can stop it, so point at the port settings.
pub fn another_user_advice(port: u16) -> String {
    format!(
        "Another Mac user is running turbofig on port {port}. Set TURBOFIG_MCP_PORT and \
         TURBOFIG_WS_PORT to free ports (see docs/configuration.md)."
    )
}

/// What to tell the user when a listener that is not their daemon holds
/// `port`. Only an owner that is another uid is called "another Mac user":
/// a non-root `lsof` cannot see their sockets, so an unknown owner gets a
/// neutral message. A same-user owner may be an older turbofig, which can be
/// quit here.
pub fn foreign_listener_advice(port: u16, owner_uid: Option<u32>, my_uid: u32) -> String {
    match owner_uid {
        Some(uid) if uid == my_uid => {
            "If this is an older turbofig, quit it (pkill -x turbofig) and retry.".to_owned()
        }
        Some(_) => another_user_advice(port),
        None => format!(
            "Something else answers on port {port} that is not this turbofig. Check what uses \
             the port (lsof -nP -iTCP:{port} -sTCP:LISTEN). If it is an app you can quit, quit \
             it and retry. Otherwise set TURBOFIG_MCP_PORT and TURBOFIG_WS_PORT to free ports \
             (see docs/configuration.md)."
        ),
    }
}

/// Reads the owner uid from `lsof -Fu` output. `None` when there is no `u`
/// field, or when the sockets have more than one owner.
pub fn parse_lsof_uid(output: &str) -> Option<u32> {
    let mut uid: Option<u32> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix('u') {
            let v: u32 = rest.trim().parse().ok()?;
            if uid.is_some_and(|u| u != v) {
                return None;
            }
            uid = Some(v);
        }
    }
    uid
}

/// How long `lsof` may run before the lookup gives up (and reads as Foreign).
/// A cold `lsof` can take about 2 s, so the limit is 5 s: an older
/// same-user daemon must not read as Foreign because `lsof` was slow.
const LSOF_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `cmd` and returns its stdout, or `None` on a spawn error, a failed
/// exit or a timeout. The child is killed when the timeout drops it.
async fn run_with_timeout(mut cmd: tokio::process::Command, limit: Duration) -> Option<String> {
    cmd.kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let out = tokio::time::timeout(limit, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads the owner uid of the listening socket on `127.0.0.1:<port>` (or
/// `0.0.0.0:<port>`) from the text of `/proc/net/tcp`. `None` when no such
/// socket is listed, or when the sockets have more than one owner.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_net_tcp_uid(table: &str, port: u16) -> Option<u32> {
    const LISTEN: &str = "0A";
    let want_port = format!("{port:04X}");
    let mut uid: Option<u32> = None;
    for line in table.lines().skip(1) {
        // sl local_address rem_address st tx:rx tr:when retrnsmt uid ...
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(local), Some(state), Some(owner)) = (f.get(1), f.get(3), f.get(7)) else {
            continue;
        };
        let Some((addr, p)) = local.split_once(':') else {
            continue;
        };
        if *state != LISTEN || p != want_port || !matches!(addr, "0100007F" | "00000000") {
            continue;
        }
        let v: u32 = owner.parse().ok()?;
        if uid.is_some_and(|u| u != v) {
            return None;
        }
        uid = Some(v);
    }
    uid
}

/// The uid that owns the listening socket on `127.0.0.1:<port>`: from
/// `lsof` on macOS, from `/proc/net/tcp` on Linux (no `lsof` there needed;
/// Linux runs only in CI). `None` when the lookup fails, times out or shows
/// nothing. Always `None` in this crate's unit tests, so a same-user mock
/// server there reads as a foreign listener.
async fn listener_uid(port: u16) -> Option<u32> {
    if cfg!(test) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let table = tokio::fs::read_to_string("/proc/net/tcp").await.ok()?;
        parse_proc_net_tcp_uid(&table, port)
    }
    #[cfg(not(target_os = "linux"))]
    listener_uid_from_lsof(port).await
}

#[cfg(not(target_os = "linux"))]
async fn listener_uid_from_lsof(port: u16) -> Option<u32> {
    let mut cmd = tokio::process::Command::new("/usr/sbin/lsof");
    cmd.args([
        "-nP",
        "-b",
        "-w",
        &format!("-iTCP@127.0.0.1:{port}"),
        "-sTCP:LISTEN",
        "-Fu",
    ]);
    parse_lsof_uid(&run_with_timeout(cmd, LSOF_TIMEOUT).await?)
}

/// The process that listens on a port, as far as `lsof` can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortOwner {
    pub pid: u32,
    pub command: String,
    pub uid: u32,
}

/// Reads the first listener from `lsof -Fpcu` output. `None` when a field is
/// missing.
pub fn parse_lsof_owner(output: &str) -> Option<PortOwner> {
    let (mut pid, mut command, mut uid) = (None, None, None);
    for line in output.lines() {
        if let Some(r) = line.strip_prefix('p') {
            if pid.is_some() {
                break;
            }
            pid = r.trim().parse().ok();
        } else if let Some(r) = line.strip_prefix('c') {
            if command.is_none() {
                command = Some(r.trim().to_owned());
            }
        } else if let Some(r) = line.strip_prefix('u') {
            if uid.is_none() {
                uid = r.trim().parse().ok();
            }
        }
    }
    Some(PortOwner {
        pid: pid?,
        command: command?,
        uid: uid?,
    })
}

/// Who listens on `127.0.0.1:<port>`, from `lsof`. `None` when `lsof` fails,
/// times out, or cannot see the socket (another user's sockets are invisible
/// to a non-root `lsof`).
pub async fn port_owner(port: u16) -> Option<PortOwner> {
    let mut cmd = tokio::process::Command::new("/usr/sbin/lsof");
    cmd.args([
        "-nP",
        "-b",
        "-w",
        &format!("-iTCP@127.0.0.1:{port}"),
        "-sTCP:LISTEN",
        "-Fpcu",
    ]);
    parse_lsof_owner(&run_with_timeout(cmd, LSOF_TIMEOUT).await?)
}

/// The startup error when `port` (`label` is "MCP" or "WS") is busy. It
/// names the port and, when `lsof` could tell, the process. An owner that is
/// another uid, or unknown, gets the settings advice: no command of this user
/// can stop it.
pub fn busy_port_message(label: &str, port: u16, owner: Option<&PortOwner>, my_uid: u32) -> String {
    let by = owner
        .map(|o| format!(" by {} (pid {})", o.command, o.pid))
        .unwrap_or_default();
    let tail = match owner {
        Some(o) if o.uid == my_uid => "Quit that process, or set TURBOFIG_MCP_PORT and \
             TURBOFIG_WS_PORT to free ports (see docs/configuration.md)."
            .to_owned(),
        _ => "It may belong to another Mac user. Set TURBOFIG_MCP_PORT and TURBOFIG_WS_PORT \
             to free ports (see docs/configuration.md)."
            .to_owned(),
    };
    format!("{label} port {port} is already in use{by}. {tail}")
}

fn port_of(base_url: &str) -> u16 {
    base_url
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
}

/// Checks that the listener at `base_url` is the daemon that holds `token`.
///
/// Sends `GET /health` with a fresh nonce and NO `Authorization` header.
/// Requires a valid proof header. Reads at most `HEALTH_BODY_CAP` bytes.
/// Returns the (unauthenticated) health body on success.
pub async fn verify_daemon(base_url: &str, token: &str) -> Result<serde_json::Value, VerifyError> {
    let client = build_client(Some(crate::spawn::ADMIN_CLIENT_TIMEOUT))
        .map_err(|_| VerifyError::Unreachable)?;
    verify_with_client(&client, base_url, token).await
}

/// `verify_daemon` through the caller's `client`. A caller that then sends
/// the token through the same client reuses the verified keep-alive
/// connection, so the token cannot go to a different listener. Only a
/// listener that proves it holds the token passes.
pub async fn verify_with_client(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<serde_json::Value, VerifyError> {
    verify_with_lookup(client, base_url, token, false, listener_uid)
        .await
        .map(|(body, _)| body)
}

/// Like `verify_with_client`, but a same-user listener with no valid proof
/// (an older turbofig daemon) also passes, and the result says which kind
/// answered. Only the upgrade handoff may use this, and only to ask an old
/// daemon to stop or restart. The token never goes to such a listener for
/// anything else, and it is never trusted for a job.
pub async fn verify_allowing_legacy(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<(serde_json::Value, Listener), VerifyError> {
    verify_with_lookup(client, base_url, token, true, listener_uid).await
}

/// `verify_allowing_legacy` for the daemon on `port`, token read from `home`.
pub async fn verify_allowing_legacy_in_home(
    port: u16,
    home: &Path,
) -> Result<(serde_json::Value, Listener), VerifyError> {
    let url = base_url(port);
    let client = build_client(Some(crate::spawn::ADMIN_CLIENT_TIMEOUT))
        .map_err(|_| VerifyError::Unreachable)?;
    match crate::token::read_token_file(home).await {
        Some(token) => verify_allowing_legacy(&client, &url, &token).await,
        None => match verify_with_client(&client, &url, "").await {
            Err(VerifyError::Unreachable) => Err(VerifyError::Unreachable),
            _ => Err(VerifyError::NoToken {
                addr: addr_of(&url),
            }),
        },
    }
}

async fn verify_with_lookup<Fut: std::future::Future<Output = Option<u32>>>(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    allow_legacy: bool,
    lookup: impl Fn(u16) -> Fut,
) -> Result<(serde_json::Value, Listener), VerifyError> {
    let addr = addr_of(base_url);
    let port = port_of(base_url);
    // The owner is looked up only on this failure path. Another uid is
    // reported as such; an unknown or same-user owner stays `Unverified`.
    let unverified = || async {
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        let my_uid = unsafe { libc::geteuid() };
        match lookup(port).await {
            Some(uid) if uid != my_uid => VerifyError::OtherUser {
                addr: addr.clone(),
                owner_uid: uid,
            },
            _ => VerifyError::Unverified { addr: addr.clone() },
        }
    };
    let nonce = new_nonce();
    let resp = match client
        .get(format!("{}/health", base_url.trim_end_matches('/')))
        .header(NONCE_HEADER, &nonce)
        .timeout(crate::spawn::ADMIN_CLIENT_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_connect() => return Err(VerifyError::Unreachable),
        // Connected, then no valid answer: a listener that is not the daemon.
        Err(_) => return Err(unverified().await),
    };
    if !resp.status().is_success() {
        return Err(unverified().await);
    }
    // The proof names the port this client connected to, so a proof from a
    // daemon on another port does not verify here.
    let signed = format!("{nonce}:{port}");
    let proof_ok = resp
        .headers()
        .get(PROOF_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| verify(token, LABEL_HTTP_DAEMON, &signed, p));
    let mut listener = Listener::Verified;
    if !proof_ok {
        // No cache: every unproven answer is classified again, so a process
        // that binds right after an old daemon exits is never taken for it.
        if !allow_legacy {
            return Err(unverified().await);
        }
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        let my_uid = unsafe { libc::geteuid() };
        listener = classify_listener(false, lookup(port).await, my_uid);
        if listener != Listener::LegacySameUser {
            return Err(unverified().await);
        }
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            eprintln!("turbofig: an older turbofig daemon is running");
        }
    }
    let Some(body) = read_json_capped(resp, HEALTH_BODY_CAP).await else {
        return Err(unverified().await);
    };
    Ok((body, listener))
}

/// `verify_daemon` for the daemon on `port`, with the token read from `home`.
/// With no token file, a reachable listener is `NoToken`.
pub async fn verify_daemon_in_home(
    port: u16,
    home: &Path,
) -> Result<serde_json::Value, VerifyError> {
    let url = base_url(port);
    match crate::token::read_token_file(home).await {
        Some(token) => verify_daemon(&url, &token).await,
        None => match verify_daemon(&url, "").await {
            Err(VerifyError::Unreachable) => Err(VerifyError::Unreachable),
            _ => Err(VerifyError::NoToken {
                addr: addr_of(&url),
            }),
        },
    }
}

/// Verifies the daemon, then reads its full `/health` payload with the token.
/// The token goes only to a verified daemon.
pub async fn verified_full_health(
    port: u16,
    token: &str,
) -> Result<serde_json::Value, ClientError> {
    let url = base_url(port);
    let client =
        build_client(Some(crate::spawn::ADMIN_CLIENT_TIMEOUT)).map_err(ClientError::Http)?;
    verify_with_client(&client, &url, token)
        .await
        .map_err(ClientError::Verify)?;
    let resp = client
        .get(format!("{url}/health"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(ClientError::Http)?;
    if !resp.status().is_success() {
        return Err(ClientError::Verify(VerifyError::Unverified {
            addr: addr_of(&url),
        }));
    }
    read_json_capped(resp, HEALTH_BODY_CAP)
        .await
        .ok_or(ClientError::Verify(VerifyError::Unverified {
            addr: addr_of(&url),
        }))
}

/// Verifies the daemon, then sends `POST /control` with the token. Returns
/// the status code only: the body is never read.
pub async fn send_control(
    port: u16,
    token: &str,
    action: &str,
) -> Result<reqwest::StatusCode, ClientError> {
    let body = serde_json::json!({ "action": action });
    send_control_with_lookup(port, token, &body, true, listener_uid).await
}

/// Asks the verified daemon to set the shared Connection (`"file"` or
/// `"mcp"`), over the verified `POST /control` path. Unlike `send_control`
/// it never allows a legacy daemon: the token goes only to a daemon that
/// proved it holds it.
pub async fn send_connection_mode(
    port: u16,
    token: &str,
    mode: &str,
) -> Result<reqwest::StatusCode, ClientError> {
    let body = serde_json::json!({ "action": "set_connection_mode", "mode": mode });
    send_control_with_lookup(port, token, &body, false, listener_uid).await
}

async fn send_control_with_lookup<Fut: std::future::Future<Output = Option<u32>>>(
    port: u16,
    token: &str,
    body: &serde_json::Value,
    allow_legacy: bool,
    lookup: impl Fn(u16) -> Fut,
) -> Result<reqwest::StatusCode, ClientError> {
    let url = base_url(port);
    let client =
        build_client(Some(crate::spawn::ADMIN_CLIENT_TIMEOUT)).map_err(ClientError::Http)?;
    // An older same-user daemon may be asked to stop or restart: that is the
    // upgrade handoff (`allow_legacy`). Nothing else sends the token to such
    // a listener.
    verify_with_lookup(&client, &url, token, allow_legacy, lookup)
        .await
        .map_err(ClientError::Verify)?;
    let resp = client
        .post(format!("{url}/control"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(ClientError::Http)?;
    Ok(resp.status())
}

/// Polls until the daemon on `port` is verified, or `deadline` passes. The
/// token is read from `home` on every poll: the daemon writes it before it
/// binds. On timeout, the message names the port and the last problem.
pub async fn wait_for_verified_daemon(
    port: u16,
    home: &Path,
    deadline: Duration,
) -> Result<serde_json::Value, String> {
    let until = Instant::now() + deadline;
    loop {
        let last = match verify_daemon_in_home(port, home).await {
            Ok(body) => return Ok(body),
            Err(e) => e,
        };
        if Instant::now() >= until {
            return Err(match last {
                VerifyError::Unreachable => {
                    format!("the daemon on port {port} did not become healthy within {deadline:?}")
                }
                other => other.to_string(),
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What `turbofig serve` does with its startup check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeCheck {
    /// Nothing listens: start the daemon.
    Proceed,
    /// A verified daemon runs, with this version: exit as "already running".
    AlreadyRunning(String),
    /// A listener that is not the daemon holds the port: log, exit non-zero.
    Foreign(String),
}

/// Decides the `serve` startup check. Only a verified daemon counts as
/// running. An unverified listener is never "already running".
pub fn serve_check(result: &Result<serde_json::Value, VerifyError>) -> ServeCheck {
    match result {
        Ok(health) => ServeCheck::AlreadyRunning(
            health
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_owned(),
        ),
        Err(VerifyError::Unreachable) => ServeCheck::Proceed,
        Err(e) => ServeCheck::Foreign(e.to_string()),
    }
}

/// A mock daemon for tests in this crate: it implements the proof contract
/// on `/health` and records every `Authorization` header it receives.
#[cfg(test)]
pub(crate) mod test_mock {
    use super::*;
    use crate::daemon_auth::proof;
    use axum::http::{HeaderMap, HeaderValue};
    use std::sync::{Arc, Mutex};

    pub(crate) const TOKEN: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// Writes `TOKEN` as `<home>/token` with mode 0600, as the daemon does.
    pub(crate) fn write_token(home: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let path = home.join("token");
        std::fs::write(&path, TOKEN).expect("token");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    #[derive(Clone, Copy)]
    pub(crate) enum Mode {
        /// Signs the nonce with `TOKEN`.
        Sign,
        /// Sends no proof header.
        NoProof,
        /// Signs with a different token.
        WrongProof,
        /// Signs correctly but sends a body over the cap.
        Huge,
    }

    #[derive(Default)]
    pub(crate) struct Seen {
        /// Every `Authorization` header value the mock received.
        pub(crate) auth: Vec<String>,
        pub(crate) requests: usize,
    }

    pub(crate) async fn mock(mode: Mode) -> (u16, Arc<Mutex<Seen>>) {
        let (port, seen, _mode) = mock_switch(mode).await;
        (port, seen)
    }

    /// Like `mock`, but the caller can change the mode while the mock runs.
    /// This stands in for "the daemon died and another process took its port".
    pub(crate) async fn mock_switch(mode: Mode) -> (u16, Arc<Mutex<Seen>>, Arc<Mutex<Mode>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let s = seen.clone();
        let mode_cell = Arc::new(Mutex::new(mode));
        let m = mode_cell.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = move |headers: HeaderMap| {
            let s = s.clone();
            let m = m.clone();
            async move {
                let mode = *m.lock().unwrap();
                {
                    let mut g = s.lock().unwrap();
                    g.requests += 1;
                    if let Some(a) = headers.get("authorization") {
                        g.auth.push(a.to_str().unwrap_or("").to_owned());
                    }
                }
                let nonce = headers
                    .get(NONCE_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let mut out = HeaderMap::new();
                let body = match mode {
                    Mode::Sign | Mode::NoProof | Mode::WrongProof => {
                        r#"{"ok":true,"version":"9.9.9"}"#.to_owned()
                    }
                    Mode::Huge => format!(r#"{{"pad":"{}"}}"#, "x".repeat(HEALTH_BODY_CAP * 2)),
                };
                let token = match mode {
                    Mode::Sign | Mode::Huge => Some(TOKEN.to_owned()),
                    Mode::WrongProof => Some("b".repeat(64)),
                    Mode::NoProof => None,
                };
                if let Some(t) = token {
                    out.insert(
                        PROOF_HEADER,
                        HeaderValue::from_str(&proof(
                            &t,
                            LABEL_HTTP_DAEMON,
                            &format!("{nonce}:{port}"),
                        ))
                        .unwrap(),
                    );
                }
                (out, body)
            }
        };
        let app = axum::Router::new()
            .route("/health", axum::routing::get(handler.clone()))
            .route("/control", axum::routing::post(handler.clone()))
            .route("/job", axum::routing::post(handler));
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (port, seen, mode_cell)
    }
}

#[cfg(test)]
mod tests {
    use super::test_mock::{mock, Mode, TOKEN};
    use super::*;

    #[tokio::test]
    async fn a_correct_proof_verifies_and_sends_no_authorization_header() {
        let (port, seen) = mock(Mode::Sign).await;
        let body = verify_daemon(&base_url(port), TOKEN)
            .await
            .expect("verified");
        assert_eq!(body["version"], "9.9.9");
        assert!(seen.lock().unwrap().auth.is_empty());
    }

    #[tokio::test]
    async fn a_missing_proof_is_unverified() {
        let (port, seen) = mock(Mode::NoProof).await;
        let err = verify_daemon(&base_url(port), TOKEN).await.unwrap_err();
        assert!(matches!(err, VerifyError::Unverified { .. }));
        assert!(err.to_string().contains(&format!("127.0.0.1:{port}")));
        assert!(err.to_string().contains("not your turbofig daemon"));
        assert!(seen.lock().unwrap().auth.is_empty());
    }

    #[tokio::test]
    async fn a_wrong_proof_is_unverified() {
        let (port, seen) = mock(Mode::WrongProof).await;
        let err = verify_daemon(&base_url(port), TOKEN).await.unwrap_err();
        assert!(matches!(err, VerifyError::Unverified { .. }));
        assert!(seen.lock().unwrap().auth.is_empty());
    }

    #[tokio::test]
    async fn an_oversize_health_body_is_refused() {
        let (port, _seen) = mock(Mode::Huge).await;
        let err = verify_daemon(&base_url(port), TOKEN).await.unwrap_err();
        assert!(matches!(err, VerifyError::Unverified { .. }));
    }

    #[tokio::test]
    async fn nothing_listening_is_unreachable() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = verify_daemon(&base_url(port), TOKEN).await.unwrap_err();
        assert_eq!(err, VerifyError::Unreachable);
    }

    #[tokio::test]
    async fn the_token_never_reaches_an_unverified_listener() {
        for mode in [Mode::NoProof, Mode::WrongProof] {
            let (port, seen) = mock(mode).await;
            assert!(verified_full_health(port, TOKEN).await.is_err());
            assert!(matches!(
                send_control(port, TOKEN, "stop").await,
                Err(ClientError::Verify(VerifyError::Unverified { .. }))
            ));
            let g = seen.lock().unwrap();
            assert!(g.requests >= 2, "the mock must have been probed");
            assert!(g.auth.is_empty(), "token leaked: {:?}", g.auth);
        }
    }

    #[tokio::test]
    async fn the_token_goes_to_a_verified_daemon() {
        let (port, seen) = mock(Mode::Sign).await;
        let status = send_control(port, TOKEN, "stop").await.expect("control");
        assert!(status.is_success());
        let g = seen.lock().unwrap();
        assert_eq!(g.auth, vec![format!("Bearer {TOKEN}")]);
    }

    #[tokio::test]
    async fn read_capped_stops_at_the_cap() {
        let (port, _seen) = mock(Mode::Huge).await;
        let client = build_client(None).unwrap();
        let resp = client
            .get(format!("{}/health", base_url(port)))
            .send()
            .await
            .unwrap();
        assert!(read_capped(resp, 1024).await.is_none());
        let resp = client
            .get(format!("{}/health", base_url(port)))
            .send()
            .await
            .unwrap();
        assert!(read_capped(resp, RESULT_BODY_CAP).await.is_some());
    }

    #[tokio::test]
    async fn no_token_file_with_a_listener_is_no_token() {
        let (port, seen) = mock(Mode::Sign).await;
        let home = tempfile::tempdir().unwrap();
        let err = verify_daemon_in_home(port, home.path()).await.unwrap_err();
        assert!(matches!(err, VerifyError::NoToken { .. }));
        assert!(seen.lock().unwrap().auth.is_empty());
    }

    #[tokio::test]
    async fn wait_for_verified_daemon_reports_a_foreign_listener_by_port() {
        let (port, _seen) = mock(Mode::NoProof).await;
        let home = tempfile::tempdir().unwrap();
        test_mock::write_token(home.path());
        let err = wait_for_verified_daemon(port, home.path(), Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
    }

    #[test]
    fn serve_check_counts_only_a_verified_daemon_as_running() {
        let ok = Ok(serde_json::json!({"version": "1.2.3"}));
        assert_eq!(serve_check(&ok), ServeCheck::AlreadyRunning("1.2.3".into()));
        assert_eq!(
            serve_check(&Err(VerifyError::Unreachable)),
            ServeCheck::Proceed
        );
        let foreign = Err(VerifyError::Unverified {
            addr: "127.0.0.1:1".into(),
        });
        assert!(
            matches!(serve_check(&foreign), ServeCheck::Foreign(m) if m.contains("127.0.0.1:1"))
        );
        let no_token = Err(VerifyError::NoToken {
            addr: "127.0.0.1:1".into(),
        });
        assert!(matches!(serve_check(&no_token), ServeCheck::Foreign(_)));
    }

    #[test]
    fn classify_listener_covers_every_case() {
        assert_eq!(classify_listener(true, None, 501), Listener::Verified);
        assert_eq!(classify_listener(true, Some(0), 501), Listener::Verified);
        assert_eq!(
            classify_listener(false, Some(501), 501),
            Listener::LegacySameUser
        );
        assert_eq!(classify_listener(false, Some(502), 501), Listener::Foreign);
        assert_eq!(classify_listener(false, None, 501), Listener::Foreign);
    }

    #[test]
    fn parse_proc_net_tcp_uid_reads_the_listener_owner() {
        let head = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        // Port 18846 = 0x499E.
        let listen = "   0: 0100007F:499E 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 111 1\n";
        let any = "   1: 00000000:499E 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 112 1\n";
        let other_port = "   2: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1002        0 113 1\n";
        let established = "   3: 0100007F:499E 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1003        0 114 1\n";
        let other_owner = "   4: 00000000:499E 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1004        0 115 1\n";
        let t = format!("{head}{listen}{other_port}{established}");
        assert_eq!(parse_proc_net_tcp_uid(&t, 18846), Some(1001));
        assert_eq!(parse_proc_net_tcp_uid(&t, 8080), Some(1002));
        assert_eq!(parse_proc_net_tcp_uid(&t, 1), None);
        let t = format!("{head}{listen}{any}");
        assert_eq!(parse_proc_net_tcp_uid(&t, 18846), Some(1001));
        let t = format!("{head}{listen}{other_owner}");
        assert_eq!(parse_proc_net_tcp_uid(&t, 18846), None);
        assert_eq!(parse_proc_net_tcp_uid(head, 18846), None);
        assert_eq!(parse_proc_net_tcp_uid("", 18846), None);
    }

    #[test]
    fn parse_lsof_uid_reads_the_u_field() {
        assert_eq!(parse_lsof_uid("p123\nu501\n"), Some(501));
        assert_eq!(parse_lsof_uid("p1\nu501\np2\nu501\n"), Some(501));
        assert_eq!(parse_lsof_uid("p1\nu501\np2\nu502\n"), None);
        assert_eq!(parse_lsof_uid(""), None);
        assert_eq!(parse_lsof_uid("uabc\n"), None);
    }

    #[tokio::test]
    async fn a_same_user_listener_without_proof_passes_only_when_legacy_is_allowed() {
        let (port, _seen) = mock(Mode::NoProof).await;
        let client = build_client(Some(Duration::from_secs(2))).unwrap();
        let my_uid = unsafe { libc::geteuid() };
        let url = base_url(port);
        for other in [Some(my_uid + 1), None] {
            let err = verify_with_lookup(&client, &url, TOKEN, true, |_| async move { other })
                .await
                .unwrap_err();
            match other {
                Some(_) => {
                    assert!(matches!(err, VerifyError::OtherUser { .. }));
                    assert!(err.to_string().contains("Another Mac user"));
                }
                None => {
                    assert!(matches!(err, VerifyError::Unverified { .. }));
                    assert!(!err.to_string().contains("Another Mac user"));
                }
            }
        }
        let strict =
            verify_with_lookup(&client, &url, TOKEN, false, |_| async move { Some(my_uid) }).await;
        assert!(matches!(strict, Err(VerifyError::Unverified { .. })));
        let (body, kind) =
            verify_with_lookup(&client, &url, TOKEN, true, |_| async move { Some(my_uid) })
                .await
                .expect("legacy is allowed");
        assert_eq!(body["version"], "9.9.9");
        assert_eq!(kind, Listener::LegacySameUser);
    }

    #[tokio::test]
    async fn the_legacy_answer_is_not_cached() {
        let (port, _seen) = mock(Mode::NoProof).await;
        let client = build_client(Some(Duration::from_secs(2))).unwrap();
        let my_uid = unsafe { libc::geteuid() };
        let url = base_url(port);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..3 {
            let r = verify_with_lookup(&client, &url, TOKEN, true, |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move { Some(my_uid) }
            })
            .await;
            assert!(r.is_ok());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        // The same listener, now owned by another user, gets nothing.
        let r = verify_with_lookup(
            &client,
            &url,
            TOKEN,
            true,
            |_| async move { Some(my_uid + 1) },
        )
        .await;
        assert!(matches!(r, Err(VerifyError::OtherUser { .. })));
    }

    #[test]
    fn the_http_proof_is_bound_to_the_port() {
        let token = "a".repeat(64);
        let nonce = "00112233445566778899aabbccddeeff";
        let want = "0aaf85667ef86027a4b34aa47ca306793f2a43d041325b92698d719121b77b35";
        assert_eq!(
            crate::daemon_auth::proof(&token, LABEL_HTTP_DAEMON, &format!("{nonce}:18846")),
            want
        );
        assert!(verify(
            &token,
            LABEL_HTTP_DAEMON,
            &format!("{nonce}:18846"),
            want
        ));
        assert!(!verify(
            &token,
            LABEL_HTTP_DAEMON,
            &format!("{nonce}:18847"),
            want
        ));
        assert!(!verify(&token, LABEL_HTTP_DAEMON, nonce, want));
    }

    #[tokio::test]
    async fn a_legacy_listener_gets_control_but_not_a_job_or_full_health() {
        let (port, seen) = mock(Mode::NoProof).await;
        let my_uid = unsafe { libc::geteuid() };
        // Full health and the strict check refuse it and send no token.
        assert!(verified_full_health(port, TOKEN).await.is_err());
        let client = build_client(Some(Duration::from_secs(2))).unwrap();
        assert!(verify_with_client(&client, &base_url(port), TOKEN)
            .await
            .is_err());
        assert!(seen.lock().unwrap().auth.is_empty());
        // send_control reaches it: the upgrade handoff.
        let status = send_control_with_lookup(
            port,
            TOKEN,
            &serde_json::json!({"action": "restart"}),
            true,
            |_| async move { Some(my_uid) },
        )
        .await
        .expect("legacy control allowed");
        assert!(status.is_success());
        assert_eq!(seen.lock().unwrap().auth, vec![format!("Bearer {TOKEN}")]);
        // A foreign owner is still refused.
        let before = seen.lock().unwrap().auth.len();
        assert!(send_control_with_lookup(
            port,
            TOKEN,
            &serde_json::json!({"action": "restart"}),
            true,
            |_| async move { Some(my_uid + 1) }
        )
        .await
        .is_err());
        assert_eq!(seen.lock().unwrap().auth.len(), before);
        // A connection-mode change (allow_legacy = false) never reaches it.
        assert!(send_control_with_lookup(
            port,
            TOKEN,
            &serde_json::json!({"action": "set_connection_mode", "mode": "mcp"}),
            false,
            |_| async move { Some(my_uid) }
        )
        .await
        .is_err());
        assert_eq!(seen.lock().unwrap().auth.len(), before);
    }

    #[tokio::test]
    async fn run_with_timeout_kills_a_hung_command_and_reads_output() {
        let mut slow = tokio::process::Command::new("/bin/sleep");
        slow.arg("5");
        let start = Instant::now();
        assert_eq!(
            run_with_timeout(slow, Duration::from_millis(100)).await,
            None
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let mut echo = tokio::process::Command::new("/bin/echo");
        echo.arg("u501");
        let out = run_with_timeout(echo, Duration::from_secs(2)).await;
        assert_eq!(out.as_deref().map(str::trim), Some("u501"));
    }

    #[test]
    fn foreign_listener_advice_says_another_mac_user_only_for_another_uid() {
        let m = foreign_listener_advice(18846, Some(999), 501);
        assert!(m.contains("Another Mac user is running turbofig on port 18846."));
        assert!(m.contains("TURBOFIG_MCP_PORT and TURBOFIG_WS_PORT"));
        assert!(m.contains("docs/configuration.md"));
        assert!(!m.contains("pkill"));
        let unknown = foreign_listener_advice(18846, None, 501);
        assert!(!unknown.contains("Another Mac user"));
        assert!(unknown.contains("not this turbofig"));
        assert!(unknown.contains("lsof -nP -iTCP:18846"));
        assert!(!unknown.contains("pkill"));
        assert!(foreign_listener_advice(18846, Some(501), 501).contains("pkill"));
    }

    #[test]
    fn unverified_error_is_neutral_and_never_advises_pkill() {
        let m = VerifyError::Unverified {
            addr: "127.0.0.1:18846".into(),
        }
        .to_string();
        assert!(m.contains("not your turbofig daemon"));
        assert!(m.contains("port 18846"));
        assert!(!m.contains("Another Mac user"));
        assert!(!m.contains("pkill"));
    }

    #[test]
    fn parse_lsof_owner_reads_pid_command_and_uid() {
        let o = parse_lsof_owner("p123\nu501\ncnode\n").expect("owner");
        assert_eq!(
            o,
            PortOwner {
                pid: 123,
                command: "node".into(),
                uid: 501
            }
        );
        assert!(parse_lsof_owner("").is_none());
        assert!(parse_lsof_owner("p1\nu5\n").is_none());
    }

    #[test]
    fn busy_port_message_names_the_port_and_the_process() {
        let mine = PortOwner {
            pid: 42,
            command: "node".into(),
            uid: 501,
        };
        let m = busy_port_message("WS", 18847, Some(&mine), 501);
        assert!(m.contains("WS port 18847 is already in use by node (pid 42)"));
        assert!(m.contains("Quit that process"));
        let other = PortOwner { uid: 7, ..mine };
        let m = busy_port_message("MCP", 18846, Some(&other), 501);
        assert!(m.contains("another Mac user"));
        let m = busy_port_message("MCP", 18846, None, 501);
        assert!(m.contains("MCP port 18846 is already in use."));
        assert!(m.contains("TURBOFIG_MCP_PORT"));
    }
}
