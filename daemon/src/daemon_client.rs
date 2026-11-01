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
    /// A listener answered, but it did not prove it holds the token.
    Unverified { addr: String },
    /// A listener answered, but there is no token file to check it against.
    NoToken { addr: String },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Unreachable => write!(f, "no turbofig daemon is reachable"),
            VerifyError::Unverified { addr } => write!(
                f,
                "a process that is not your turbofig daemon is listening on {addr}. \
                 If this is an older turbofig, quit it (pkill -x turbofig) and retry."
            ),
            VerifyError::NoToken { addr } => write!(
                f,
                "a process is listening on {addr}, but there is no pairing token file to verify it against"
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
const LSOF_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a "same-user older daemon" answer is reused for one port. It
/// saves an `lsof` per proxy job and per menu-bar poll. Any other answer
/// for that port clears it.
const LEGACY_CACHE_TTL: Duration = Duration::from_secs(30);

fn legacy_cache() -> &'static std::sync::Mutex<std::collections::HashMap<u16, Instant>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u16, Instant>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

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

/// The uid that owns the listening socket on `127.0.0.1:<port>`, from
/// `lsof`. `None` when `lsof` fails, times out or shows nothing. Always
/// `None` in this crate's unit tests, so a same-user mock server there
/// reads as a foreign listener.
async fn listener_uid(port: u16) -> Option<u32> {
    if cfg!(test) {
        return None;
    }
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

fn clear_legacy_cache(port: u16) {
    if let Ok(mut c) = legacy_cache().lock() {
        c.remove(&port);
    }
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
/// connection, so the token cannot go to a different listener.
pub async fn verify_with_client(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<serde_json::Value, VerifyError> {
    verify_with_lookup(client, base_url, token, listener_uid).await
}

async fn verify_with_lookup<Fut: std::future::Future<Output = Option<u32>>>(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    lookup: impl Fn(u16) -> Fut,
) -> Result<serde_json::Value, VerifyError> {
    let addr = addr_of(base_url);
    let unverified = || VerifyError::Unverified { addr: addr.clone() };
    let nonce = new_nonce();
    let resp = match client
        .get(format!("{}/health", base_url.trim_end_matches('/')))
        .header(NONCE_HEADER, &nonce)
        .timeout(crate::spawn::ADMIN_CLIENT_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_connect() => {
            clear_legacy_cache(port_of(base_url));
            return Err(VerifyError::Unreachable);
        }
        // Connected, then no valid answer: a listener that is not the daemon.
        Err(_) => return Err(unverified()),
    };
    if !resp.status().is_success() {
        return Err(unverified());
    }
    let proof_ok = resp
        .headers()
        .get(PROOF_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| verify(token, LABEL_HTTP_DAEMON, &nonce, p));
    if !proof_ok {
        let port = port_of(base_url);
        let cached = legacy_cache()
            .lock()
            .ok()
            .and_then(|c| c.get(&port).copied())
            .is_some_and(|t| t.elapsed() < LEGACY_CACHE_TTL);
        let listener = if cached {
            Listener::LegacySameUser
        } else {
            // SAFETY: `geteuid` takes no arguments and cannot fail.
            let my_uid = unsafe { libc::geteuid() };
            classify_listener(false, lookup(port).await, my_uid)
        };
        match listener {
            Listener::LegacySameUser => {
                if let Ok(mut c) = legacy_cache().lock() {
                    c.entry(port).or_insert_with(Instant::now);
                }
                static LOGGED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !LOGGED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    eprintln!("turbofig: an older turbofig daemon is running");
                }
            }
            _ => {
                clear_legacy_cache(port);
                return Err(unverified());
            }
        }
    } else {
        clear_legacy_cache(port_of(base_url));
    }
    read_json_capped(resp, HEALTH_BODY_CAP)
        .await
        .ok_or_else(unverified)
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
    let url = base_url(port);
    let client =
        build_client(Some(crate::spawn::ADMIN_CLIENT_TIMEOUT)).map_err(ClientError::Http)?;
    verify_with_client(&client, &url, token)
        .await
        .map_err(ClientError::Verify)?;
    let resp = client
        .post(format!("{url}/control"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "action": action }))
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
                        HeaderValue::from_str(&proof(&t, LABEL_HTTP_DAEMON, &nonce)).unwrap(),
                    );
                }
                (out, body)
            }
        };
        let app = axum::Router::new()
            .route("/health", axum::routing::get(handler.clone()))
            .route("/control", axum::routing::post(handler.clone()))
            .route("/job", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
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
        tokio::fs::write(home.path().join("token"), TOKEN)
            .await
            .unwrap();
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
    fn parse_lsof_uid_reads_the_u_field() {
        assert_eq!(parse_lsof_uid("p123\nu501\n"), Some(501));
        assert_eq!(parse_lsof_uid("p1\nu501\np2\nu501\n"), Some(501));
        assert_eq!(parse_lsof_uid("p1\nu501\np2\nu502\n"), None);
        assert_eq!(parse_lsof_uid(""), None);
        assert_eq!(parse_lsof_uid("uabc\n"), None);
    }

    #[tokio::test]
    async fn a_same_user_listener_without_proof_is_a_legacy_daemon() {
        let (port, _seen) = mock(Mode::NoProof).await;
        let client = build_client(Some(Duration::from_secs(2))).unwrap();
        let my_uid = unsafe { libc::geteuid() };
        // Foreign answers first: they must not leave a cached legacy result.
        for other in [Some(my_uid + 1), None] {
            let err = verify_with_lookup(&client, &base_url(port), TOKEN, |_| async move { other })
                .await
                .unwrap_err();
            assert!(matches!(err, VerifyError::Unverified { .. }));
            assert!(err.to_string().contains("pkill -x turbofig"));
        }
        let ok = verify_with_lookup(
            &client,
            &base_url(port),
            TOKEN,
            |_| async move { Some(my_uid) },
        )
        .await;
        assert_eq!(ok.expect("legacy is allowed")["version"], "9.9.9");
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

    #[tokio::test]
    async fn a_legacy_answer_is_cached_so_lsof_runs_once() {
        let (port, _seen) = mock(Mode::NoProof).await;
        let client = build_client(Some(Duration::from_secs(2))).unwrap();
        let my_uid = unsafe { libc::geteuid() };
        let calls = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..3 {
            let r = verify_with_lookup(&client, &base_url(port), TOKEN, |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move { Some(my_uid) }
            })
            .await;
            assert!(r.is_ok());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
