//! The second-instance signal: a Unix socket at `<home>/app.sock`, mode
//! `0600`. A second menu-bar launch, once `lock::try_acquire` tells it
//! another instance already holds `<home>/app.lock`, connects to this
//! socket, sends `OPEN_ABOUT_MESSAGE`, and exits 0; the first instance's
//! listener thread forwards that into the tao event loop as a request to
//! open (or focus) the About window.

use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// The only message this socket ever carries.
pub const OPEN_ABOUT_MESSAGE: &str = "open_about";

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

/// Sends the open-about signal to an already-running instance's socket at
/// `path`. `Ok(true)`: a listener accepted it. `Ok(false)`: nothing is
/// listening there (a stale or missing socket) — this should not normally
/// happen, since the caller only reaches this after `lock::try_acquire`
/// already found the lock held, but a missing listener must still be a
/// quiet no-op, not a crash, for the second instance's exit to stay clean.
pub fn signal_open_about(path: &Path) -> io::Result<bool> {
    match UnixStream::connect(path) {
        Ok(mut stream) => {
            stream.write_all(OPEN_ABOUT_MESSAGE.as_bytes())?;
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

/// Reads one message off an accepted connection and reports whether it was
/// exactly the open-about signal; any other content (or a read failure) is
/// not, and is ignored by the caller.
pub fn read_is_open_about(mut stream: UnixStream) -> bool {
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(n) => &buf[..n] == OPEN_ABOUT_MESSAGE.as_bytes(),
        Err(_) => false,
    }
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
            read_is_open_about(stream)
        });

        let sent = signal_open_about(&socket_path).expect("signal_open_about");
        assert!(sent, "a listener was bound, so the signal must be accepted");

        assert!(accepted.join().expect("join accept thread"));
    }

    #[test]
    fn signalling_with_no_listener_reports_false_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket_path = dir.path().join("app.sock");

        let sent = signal_open_about(&socket_path).expect("signal_open_about");
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
}
