//! The second-instance signal: a Unix socket at `<home>/app.sock`, mode
//! `0600`, carrying exactly 1 of 2 fixed literal messages. A second
//! menu-bar launch, once `lock::try_acquire` tells it another instance
//! already holds `<home>/app.lock`, connects to this socket and sends
//! `SignalMessage::OpenAbout`, then exits 0; the first instance's listener
//! thread forwards that into the tao event loop as a request to open (or
//! focus) the About window. `turbofig uninstall` sends
//! `SignalMessage::Quit` the same way, to ask a running app to quit itself
//! before uninstall's own steps proceed.

use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// The 2 messages this socket ever carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalMessage {
    /// Open the About window, or focus it if it is already open.
    OpenAbout,
    /// Quit the app (same as its own "Quit Turbofig" menu item).
    Quit,
}

impl SignalMessage {
    fn as_str(self) -> &'static str {
        match self {
            SignalMessage::OpenAbout => "open_about",
            SignalMessage::Quit => "quit_app",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "open_about" => Some(SignalMessage::OpenAbout),
            "quit_app" => Some(SignalMessage::Quit),
            _ => None,
        }
    }
}

/// Binds a fresh listening socket at `path`, mode `0600`. Removes a stale
/// socket file left by a previous crashed instance first: `UnixListener::bind`
/// fails outright if the path already exists, and a crash never gets a
/// chance to clean its own socket up.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Sends `message` to an already-running instance's socket at `path`.
/// `Ok(true)`: a listener accepted it. `Ok(false)`: nothing is listening
/// there (a stale or missing socket) — for the open-about path this should
/// not normally happen, since the caller only reaches this after
/// `lock::try_acquire` already found the lock held; for the quit path
/// (`turbofig uninstall`) it is the ordinary case when the app was never
/// running at all. Either way a missing listener is a quiet no-op, not a
/// crash.
pub fn send(path: &Path, message: SignalMessage) -> io::Result<bool> {
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            stream.write_all(message.as_str().as_bytes())?;
            Ok(true)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Reads one message off an accepted connection and parses it; `None` for
/// anything else (a read failure, or content that is not one of the 2 known
/// messages), which the caller ignores rather than acting on.
pub fn read(mut stream: UnixStream) -> Option<SignalMessage> {
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).ok()?;
    SignalMessage::parse(std::str::from_utf8(&buf[..n]).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_sent_by_a_second_instance_is_received_by_the_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let listener = bind(&socket_path).expect("bind");
        let accepted = std::thread::spawn(move || {
            let (stream, _addr) = listener.accept().expect("accept");
            read(stream)
        });

        let sent = send(&socket_path, SignalMessage::OpenAbout).expect("send");
        assert!(sent, "a listener was bound, so the signal must be accepted");

        assert_eq!(
            accepted.join().expect("join accept thread"),
            Some(SignalMessage::OpenAbout)
        );
    }

    #[test]
    fn a_quit_signal_is_received_distinctly_from_open_about() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let listener = bind(&socket_path).expect("bind");
        let accepted = std::thread::spawn(move || {
            let (stream, _addr) = listener.accept().expect("accept");
            read(stream)
        });

        send(&socket_path, SignalMessage::Quit).expect("send");

        assert_eq!(
            accepted.join().expect("join accept thread"),
            Some(SignalMessage::Quit)
        );
    }

    #[test]
    fn signalling_with_no_listener_reports_false_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let sent = send(&socket_path, SignalMessage::OpenAbout).expect("send");
        assert!(!sent, "no listener was ever bound at this path");
    }

    #[test]
    fn the_socket_file_is_created_with_mode_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let _listener = bind(&socket_path).expect("bind");
        let mode = std::fs::metadata(&socket_path)
            .expect("stat socket")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn binding_again_replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let first = bind(&socket_path).expect("first bind");
        drop(first);
        // The socket file is still on disk (nothing removed it), simulating
        // a stale file left by a crashed instance.
        assert!(socket_path.exists());

        let _second = bind(&socket_path).expect("second bind must replace the stale file");
    }

    #[test]
    fn an_unrecognised_message_parses_to_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let listener = bind(&socket_path).expect("bind");
        let accepted = std::thread::spawn(move || {
            let (stream, _addr) = listener.accept().expect("accept");
            read(stream)
        });

        let mut stream = UnixStream::connect(&socket_path).expect("connect");
        stream.write_all(b"garbage").expect("write garbage");
        drop(stream);

        assert_eq!(accepted.join().expect("join accept thread"), None);
    }
}
