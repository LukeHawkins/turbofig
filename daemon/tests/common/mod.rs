//! Shared helpers for the daemon integration tests.
//!
//! `wait_until` replaces a fixed sleep with a condition poll: a fast machine
//! returns as soon as the condition is true, and a slow CI runner still gets
//! up to `deadline_ms` before the test panics. This removes the flake class
//! where a fixed sleep (tuned on a fast dev machine) is too short on a
//! several-times-slower runner.
//!
//! This module is compiled fresh into every test binary that declares
//! `mod common;`, and no single binary uses every helper here, so dead code
//! is expected and allowed rather than a real warning.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Generous default deadline for a condition wait. A healthy condition
/// resolves in a few milliseconds; this only bounds a genuine hang.
pub const WAIT_DEADLINE_MS: u64 = 5000;

/// Poll interval while a condition is still false.
const POLL_INTERVAL_MS: u64 = 10;

/// Poll `condition` until it returns true, or panic with `msg` once
/// `deadline_ms` elapses.
pub async fn wait_until<F>(mut condition: F, deadline_ms: u64, msg: &str)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        if condition() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("Timed out waiting for: {msg}");
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

/// Poll for a file to appear and return its contents, up to `deadline_ms`.
/// Panics on timeout. Shared by every test that reads a bridge outbox result.
pub async fn poll_file(path: &Path, deadline_ms: u64) -> String {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => return contents,
            Err(_) => {
                if Instant::now() >= deadline {
                    panic!("Timed out waiting for {}", path.display());
                }
                tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            }
        }
    }
}

/// Wait until `state` shows a connection registered under `file_key`.
/// Replaces the fixed post-FILE_INFO sleep every mock-plugin helper used.
pub async fn wait_for_file_key(state: &Arc<turbofig::AppState>, file_key: &str, deadline_ms: u64) {
    let key = file_key.to_owned();
    wait_until(
        || state.list_connections().iter().any(|(_, fk, _)| fk == &key),
        deadline_ms,
        &format!("plugin to register fileKey {file_key}"),
    )
    .await;
}

/// Wait until `state` shows at least `n` connections with a non-empty file
/// key registered. Use this where a test does not care which key, only that
/// enough plugins have announced themselves.
pub async fn wait_for_connection_count(
    state: &Arc<turbofig::AppState>,
    n: usize,
    deadline_ms: u64,
) {
    wait_until(
        || {
            state
                .list_connections()
                .iter()
                .filter(|(_, fk, _)| !fk.is_empty())
                .count()
                >= n
        },
        deadline_ms,
        &format!("{n} plugin(s) to register"),
    )
    .await;
}

/// Wait until `state` shows no connections at all. Use this after closing a
/// mock plugin socket, to replace a fixed "allow the close to land" sleep.
pub async fn wait_for_no_connections(state: &Arc<turbofig::AppState>, deadline_ms: u64) {
    wait_until(
        || state.list_connections().is_empty(),
        deadline_ms,
        "all connections to close",
    )
    .await;
}
