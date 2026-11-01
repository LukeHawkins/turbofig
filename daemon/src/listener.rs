//! A hardened listener for `axum::serve`.
//!
//! Another local account can open many half-open connections. Each one holds a
//! file descriptor. Without a limit, they use all the descriptors and block
//! both ports and the file bridge. This listener adds two limits:
//!
//! - A cap on concurrent connections. The listener does not accept connection
//!   N+1 until one of the first N closes.
//! - An idle timer. Every read and every write resets it. A connection that
//!   neither reads nor writes for the whole limit is closed. This ends slow
//!   header attacks and idle keep-alive connections. The timer stops when the
//!   server sends a `101` response, so the plugin WebSocket stays open once
//!   upgraded.
//! - The timer does not run while a request is in flight. A tool call can run
//!   for minutes with no bytes in either direction. `track_inflight` counts
//!   the requests of each connection, and the connection shares the count.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::serve::Listener;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, Sleep};

/// Most concurrent connections on the HTTP port.
pub(crate) const HTTP_MAX_CONNECTIONS: usize = 64;
/// Most concurrent connections on the WebSocket port.
pub(crate) const WS_MAX_CONNECTIONS: usize = 32;
/// Idle limit on the HTTP port. It is above the 15 s SSE keep-alive of rmcp,
/// so a stream that the server keeps writing stays open. Tests use a short
/// limit so the serve functions can be tested.
pub(crate) const HTTP_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(600)
} else {
    Duration::from_secs(30)
};
/// Idle limit on the WebSocket port, before the upgrade.
pub(crate) const WS_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(600)
} else {
    Duration::from_secs(10)
};

/// A `TcpListener` with a connection cap and an idle timer.
pub(crate) struct LimitedListener {
    inner: TcpListener,
    permits: Arc<Semaphore>,
    idle_timeout: Duration,
}

impl LimitedListener {
    pub(crate) fn new(inner: TcpListener, max_connections: usize, idle_timeout: Duration) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(max_connections)),
            idle_timeout,
        }
    }
}

/// What the listener knows about one connection: the peer and the number of
/// requests in flight on it. The connection and `track_inflight` share the count.
#[derive(Clone, Debug)]
pub(crate) struct ConnInfo {
    #[allow(dead_code)]
    pub(crate) peer: std::net::SocketAddr,
    inflight: Arc<AtomicUsize>,
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, LimitedListener>>
    for ConnInfo
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, LimitedListener>) -> Self {
        stream.remote_addr().clone()
    }
}

/// Decrements the in-flight count when dropped, so a cancelled request counts too.
struct InflightGuard(Arc<AtomicUsize>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Middleware: count the request as in flight until its handler finishes.
pub(crate) async fn track_inflight(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let _guard = req
        .extensions()
        .get::<axum::extract::ConnectInfo<ConnInfo>>()
        .map(|info| {
            info.0.inflight.fetch_add(1, Ordering::SeqCst);
            InflightGuard(info.0.inflight.clone())
        });
    next.run(req).await
}

impl Listener for LimitedListener {
    type Io = LimitedIo;
    type Addr = ConnInfo;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        // Wait for a free slot before accepting, so the kernel queue holds
        // the extra connections and the process opens no extra descriptor.
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("the semaphore is never closed");
        let (stream, addr) = Listener::accept(&mut self.inner).await;
        let inflight = Arc::new(AtomicUsize::new(0));
        let io = LimitedIo {
            stream,
            inflight: inflight.clone(),
            _permit: permit,
            deadline: Some(Box::pin(tokio::time::sleep_until(
                Instant::now() + self.idle_timeout,
            ))),
            idle_timeout: self.idle_timeout,
        };
        (
            io,
            ConnInfo {
                peer: addr,
                inflight,
            },
        )
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(ConnInfo {
            peer: self.inner.local_addr()?,
            inflight: Arc::new(AtomicUsize::new(0)),
        })
    }
}

/// A connection that holds one slot and an idle timer.
pub(crate) struct LimitedIo {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
    /// Requests in flight on this connection.
    inflight: Arc<AtomicUsize>,
    /// `Some` until the server sends a `101` response.
    deadline: Option<Pin<Box<Sleep>>>,
    idle_timeout: Duration,
}

impl LimitedIo {
    /// Count a write as activity. A `101` response ends the timer for good.
    fn after_write(&mut self, written: usize, data: &[u8]) {
        if written > 0 && data.starts_with(b"HTTP/1.1 101") {
            self.deadline = None;
        } else {
            self.reset_timer();
        }
    }

    fn reset_timer(&mut self) {
        let next = Instant::now() + self.idle_timeout;
        if let Some(deadline) = self.deadline.as_mut() {
            deadline.as_mut().reset(next);
        }
    }
}

impl AsyncRead for LimitedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let busy = this.inflight.load(Ordering::SeqCst) > 0;
        if busy {
            // A request is in flight: no limit. Start a fresh deadline.
            this.reset_timer();
        }
        if let Some(deadline) = this.deadline.as_mut() {
            if deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "connection idle for too long",
                )));
            }
        }
        let res = Pin::new(&mut this.stream).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = res {
            this.reset_timer();
        }
        res
    }
}

impl AsyncWrite for LimitedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let res = Pin::new(&mut this.stream).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = res {
            this.after_write(n, buf);
        }
        res
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        let res = Pin::new(&mut this.stream).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = res {
            let first: &[u8] = bufs.first().map_or(&[], |b| &b[..]);
            this.after_write(n, first);
        }
        res
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const REQ: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";

    async fn start(max: usize, idle: Duration) -> std::net::SocketAddr {
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = tcp.local_addr().expect("addr");
        let listener = LimitedListener::new(tcp, max, idle);
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .route("/sse", axum::routing::get(sse_like))
            .route("/ws", axum::routing::get(ws_echo))
            .route("/slow", axum::routing::get(slow))
            .layer(axum::middleware::from_fn(track_inflight))
            .into_make_service_with_connect_info::<ConnInfo>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        addr
    }

    /// A body that the server keeps writing: one chunk every 100 ms.
    async fn sse_like() -> axum::response::Response {
        let stream = futures_util::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Some((Ok::<_, std::convert::Infallible>(": ping\n\n"), ()))
        });
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stream))
            .expect("response")
    }

    /// A handler that is silent for 1 s, longer than the 300 ms test limit.
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        "slow ok"
    }

    async fn ws_echo(ws: axum::extract::ws::WebSocketUpgrade) -> axum::response::Response {
        ws.on_upgrade(|mut s| async move { while s.recv().await.is_some() {} })
    }

    async fn get_ok(addr: std::net::SocketAddr) -> String {
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        let mut out = String::new();
        s.read_to_string(&mut out).await.expect("read");
        out
    }

    /// True when the server closes `s` within `limit`.
    async fn closed_within(s: &mut TcpStream, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout_at(end, s.read(&mut buf)).await {
                Err(_) => return false,
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) => continue,
            }
        }
    }

    #[tokio::test]
    async fn a_normal_request_works() {
        let addr = start(2, Duration::from_secs(5)).await;
        assert!(get_ok(addr).await.starts_with("HTTP/1.1 200"));
    }

    #[tokio::test]
    async fn an_idle_connection_with_no_headers_is_closed_after_the_timeout() {
        let addr = start(2, Duration::from_millis(200)).await;
        let mut s = TcpStream::connect(addr).await.expect("connect");
        assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn a_slow_partial_header_is_closed_after_the_timeout() {
        let addr = start(2, Duration::from_millis(300)).await;
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(b"GET / HTTP/1.1\r\n").await.expect("write");
        assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn an_idle_keep_alive_connection_is_closed_after_one_request() {
        let addr = start(2, Duration::from_millis(300)).await;
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(REQ).await.expect("write");
        let mut buf = [0u8; 256];
        let n = s.read(&mut buf).await.expect("read");
        assert!(n > 0);
        assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn a_connection_the_server_keeps_writing_stays_open() {
        let addr = start(2, Duration::from_millis(300)).await;
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(b"GET /sse HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        // Read for 1.2 s, four times the idle limit. The stream must live.
        assert!(!closed_within(&mut s, Duration::from_millis(1200)).await);
    }

    #[tokio::test]
    async fn an_upgraded_socket_stays_open_past_the_limit() {
        let addr = start(2, Duration::from_millis(300)).await;
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(
            b"GET /ws HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
              Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .await
        .expect("write");
        let mut buf = [0u8; 512];
        let n = s.read(&mut buf).await.expect("read");
        assert!(std::str::from_utf8(&buf[..n])
            .expect("utf8")
            .starts_with("HTTP/1.1 101"));
        assert!(!closed_within(&mut s, Duration::from_millis(1200)).await);
    }

    #[tokio::test]
    async fn a_slow_request_survives_the_idle_limit_then_idle_is_closed() {
        let addr = start(2, Duration::from_millis(300)).await;
        let mut s = TcpStream::connect(addr).await.expect("connect");
        s.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        let mut buf = [0u8; 512];
        let n = tokio::time::timeout(Duration::from_secs(3), s.read(&mut buf))
            .await
            .expect("the slow response must arrive")
            .expect("read");
        assert!(std::str::from_utf8(&buf[..n])
            .expect("utf8")
            .starts_with("HTTP/1.1 200"));
        // The request is done. The idle limit applies again.
        assert!(closed_within(&mut s, Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn the_connection_over_the_cap_waits_until_a_slot_frees() {
        let addr = start(2, Duration::from_secs(30)).await;
        let a = TcpStream::connect(addr).await.expect("a");
        let b = TcpStream::connect(addr).await.expect("b");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut c = TcpStream::connect(addr).await.expect("c");
        c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        let mut buf = [0u8; 64];
        let early = tokio::time::timeout(Duration::from_millis(400), c.read(&mut buf)).await;
        assert!(early.is_err(), "c must not be served while the cap is full");
        drop(a);
        let n = tokio::time::timeout(Duration::from_secs(3), c.read(&mut buf))
            .await
            .expect("c must be served once a slot frees")
            .expect("read");
        assert!(n > 0);
        drop(b);
    }

    /// Wiring: the real HTTP serve function must use the limited listener.
    #[tokio::test]
    async fn serve_with_state_closes_a_connection_that_sends_no_headers() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = tcp.local_addr().expect("addr");
        let state = Arc::new(crate::state::AppState::new());
        tokio::spawn(async move {
            let _ = crate::mcp::serve_with_state(tcp, state).await;
        });
        let mut s = TcpStream::connect(addr).await.expect("connect");
        assert!(closed_within(&mut s, HTTP_IDLE_TIMEOUT * 5).await);
    }

    /// Wiring: the real WebSocket serve function must use the limited listener.
    #[tokio::test]
    async fn serve_ws_closes_a_connection_that_sends_no_headers() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = tcp.local_addr().expect("addr");
        let state = Arc::new(crate::state::AppState::new());
        tokio::spawn(async move {
            let _ = crate::ws::serve_ws(tcp, state).await;
        });
        let mut s = TcpStream::connect(addr).await.expect("connect");
        assert!(closed_within(&mut s, WS_IDLE_TIMEOUT * 5).await);
    }
}
