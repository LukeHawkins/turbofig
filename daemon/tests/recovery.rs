//! Recovery behaviour of a real daemon: cold-start routing grace, a deleted
//! token file, and stale inbox jobs. Temp HOME and free ports only.

mod common;

use common::{
    connect_plugin, fetch_health_with_token, poll_file, spawn_daemon, wait_for_health, DaemonGuard,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as Tt;

struct Rig {
    mcp_port: u16,
    ws_port: u16,
    _child: tokio::process::Child,
    _guard: DaemonGuard,
    home: tempfile::TempDir,
    /// Taken just before the daemon spawns, so it is never later than the
    /// daemon's own start.
    spawned: Instant,
    /// Last field: dropped last, after the daemon is gone. Tests that spawn
    /// real daemons run one at a time.
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn start_daemon_in(home: tempfile::TempDir) -> Rig {
    let serial = common::serial_process_test().await;
    let (mcp_port, ws_port) = common::free_port_pair();
    let guard = DaemonGuard::new(home.path(), mcp_port);
    let spawned = Instant::now();
    let child = spawn_daemon(home.path(), mcp_port, ws_port, None);
    wait_for_health(&client(), mcp_port).await;
    let mut guard = guard;
    guard.refresh_pid().await;
    Rig {
        mcp_port,
        ws_port,
        _child: child,
        _guard: guard,
        home,
        spawned,
        _serial: serial,
    }
}

async fn start_daemon() -> Rig {
    start_daemon_in(tempfile::tempdir().expect("temp home")).await
}

fn write_job(home: &Path, id: &str) {
    let inbox = home.join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    std::fs::write(
        inbox.join(format!("{id}.json")),
        json!({"op": "execute", "code": "return \"ok\";"}).to_string(),
    )
    .expect("job");
}

/// Connects a mock plugin that answers EXECUTE with ok. Returns a task
/// handle and a counter of EXECUTE frames it received.
async fn connect_answering_plugin(
    rig: &Rig,
) -> (
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let token = std::fs::read_to_string(rig.home.path().join("token")).expect("token");
    let mut ws = connect_plugin(rig.ws_port, token.trim()).await;
    ws.send(Tt::Text(
        json!({"type": "FILE_INFO", "fileKey": "RecFile", "name": "Rec"}).to_string(),
    ))
    .await
    .expect("FILE_INFO");
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen2 = seen.clone();
    let task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            let text = match msg {
                Tt::Text(t) => t,
                Tt::Ping(p) => {
                    let _ = ws.send(Tt::Pong(p)).await;
                    continue;
                }
                _ => continue,
            };
            let Ok(frame) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(id) = frame.get("requestId").and_then(Value::as_u64) else {
                continue;
            };
            if frame.get("type").and_then(Value::as_str) == Some("EXECUTE") {
                seen2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let reply = json!({"type": "RESULT", "requestId": id, "ok": true,
                                   "result": {"marker": "ok"}});
                if ws.send(Tt::Text(reply.to_string())).await.is_err() {
                    break;
                }
            }
        }
    });
    (task, seen)
}

async fn result_of(home: &Path, id: &str, deadline_ms: u64) -> Value {
    let text = poll_file(&home.join("outbox").join(format!("{id}.json")), deadline_ms).await;
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

#[tokio::test]
async fn a_job_sent_before_the_plugin_connects_succeeds_within_the_grace() {
    let rig = start_daemon().await;
    write_job(rig.home.path(), "cold1");
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (task, _seen) = connect_answering_plugin(&rig).await;
    let v = result_of(rig.home.path(), "cold1", 8_000).await;
    assert_eq!(v["ok"], json!(true), "{v}");
    task.abort();
    let _ = task.await;
    wait_for_no_connections_via_health(&rig).await;
}

async fn wait_for_no_connections_via_health(rig: &Rig) {
    let token = std::fs::read_to_string(rig.home.path().join("token")).expect("token");
    let c = client();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let h = fetch_health_with_token(&c, rig.mcp_port, token.trim()).await;
        if h.is_some_and(|h| h["connectedFiles"].as_array().is_none_or(|a| a.is_empty())) {
            return;
        }
        assert!(Instant::now() < deadline, "plugin never disconnected");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_job_fails_after_the_grace_when_no_plugin_ever_connects() {
    let rig = start_daemon().await;
    // The grace ends when the daemon is 5 s old, or 3 s after the job
    // arrives, whichever is first. The daemon is at most `age_at_job` old
    // here, so the answer cannot come sooner than this.
    let t = Instant::now();
    let age_at_job = rig.spawned.elapsed();
    write_job(rig.home.path(), "cold2");
    let v = result_of(rig.home.path(), "cold2", 15_000).await;
    assert_eq!(v["code"], json!("plugin_disconnected"), "{v}");
    let min_wait = Duration::from_millis(3_000)
        .min(Duration::from_millis(5_000).saturating_sub(age_at_job))
        .saturating_sub(Duration::from_millis(100));
    assert!(
        t.elapsed() >= min_wait,
        "must wait out the grace first: {:?} (expected at least {min_wait:?})",
        t.elapsed()
    );
}

#[tokio::test]
async fn a_job_fails_fast_once_a_plugin_had_connected_and_left() {
    let rig = start_daemon().await;
    let (task, _seen) = connect_answering_plugin(&rig).await;
    write_job(rig.home.path(), "warm");
    assert_eq!(
        result_of(rig.home.path(), "warm", 8_000).await["ok"],
        json!(true)
    );
    task.abort();
    let _ = task.await;
    wait_for_no_connections_via_health(&rig).await;
    let t = Instant::now();
    write_job(rig.home.path(), "gone");
    let v = result_of(rig.home.path(), "gone", 8_000).await;
    assert_eq!(v["code"], json!("plugin_disconnected"), "{v}");
    assert!(
        t.elapsed() < Duration::from_millis(2_500),
        "no grace once a plugin had connected: {:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn a_deleted_token_file_is_written_back_with_the_same_token() {
    let rig = start_daemon().await;
    let path = rig.home.path().join("token");
    let before = std::fs::read_to_string(&path).expect("token");
    std::fs::remove_file(&path).expect("rm token");
    common::wait_until(|| path.exists(), 5_000, "token file restored").await;
    // Wait for the write to finish (the file appears whole via a hard link).
    let after = std::fs::read_to_string(&path).expect("token again");
    assert_eq!(before, after);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let h = fetch_health_with_token(&client(), rig.mcp_port, after.trim()).await;
    assert!(h.is_some(), "the restored token must authenticate");
}

#[tokio::test]
async fn a_stale_inbox_job_gets_not_started_and_never_reaches_the_plugin() {
    let home = tempfile::tempdir().expect("temp home");
    write_job(home.path(), "old1");
    let f = std::fs::File::options()
        .write(true)
        .open(home.path().join("inbox/old1.json"))
        .expect("open job");
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(3 * 3600))
        .expect("age the job");
    drop(f);
    let rig = start_daemon_in(home).await;
    let v = result_of(rig.home.path(), "old1", 5_000).await;
    assert_eq!(v["code"], json!("not_started"), "{v}");
    assert!(v["error"].as_str().unwrap().contains("too old"), "{v}");
    assert!(!rig.home.path().join("inbox/old1.json").exists());
    let (task, seen) = connect_answering_plugin(&rig).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 0);
    // A fresh job still runs.
    write_job(rig.home.path(), "new1");
    assert_eq!(
        result_of(rig.home.path(), "new1", 8_000).await["ok"],
        json!(true)
    );
    task.abort();
    let _ = task.await;
}
