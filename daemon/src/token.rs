//! The WebSocket pairing token.
//!
//! A sandboxed `<iframe>` on any web page reports Origin `null`, the same
//! value the real Figma plugin UI reports (see `ws.rs`). Origin checking
//! alone cannot tell the two apart, so a malicious page could open the
//! plugin WebSocket and receive the AI's jobs. The pairing token closes that
//! gap: the WS upgrade now also requires a `token` query parameter matching
//! the value in `<home>/token`.
//!
//! Token generation (`random_token_hex`) is split from token persistence
//! (`ensure_token`) on purpose. `AppState::new`/`with_timeout` call
//! `random_token_hex` directly, entirely in memory, so a unit or integration
//! test that builds an `AppState` never touches a real `~/.turbofig`. Only
//! the daemon binary (`main.rs`) calls `ensure_token`, which is the one path
//! that reads or writes the real token file.

use std::io;
use std::path::Path;

/// Number of random bytes in a token. Hex-encoded, this is a 64-character string.
const TOKEN_BYTES: usize = 32;

/// Generates `TOKEN_BYTES` of randomness and returns it hex-encoded.
///
/// No `rand`/`getrandom` dependency: `std::collections::hash_map::RandomState`
/// draws a fresh SipHash key from the OS CSPRNG on every `RandomState::new()`
/// call (see `state::random_counter_start`, which uses the same technique for
/// the request-id counter seed). Hashing a fixed, empty input through 4
/// independently-seeded hashers yields 4 unrelated `u64`s, i.e. 32 bytes
/// derived from 256 bits of OS randomness, with no new crate.
pub(crate) fn random_token_hex() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = Vec::with_capacity(TOKEN_BYTES);
    while bytes.len() < TOKEN_BYTES {
        let word = RandomState::new().build_hasher().finish();
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.truncate(TOKEN_BYTES);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compares two strings in constant time with respect to their shared length.
///
/// A length mismatch returns `false` immediately: the expected token length
/// is fixed and public (the daemon always generates a 64-character hex
/// token), so leaking "this candidate is the wrong length" is not a
/// meaningful side channel. What matters is that a same-length wrong guess
/// cannot be distinguished by timing from a near-miss.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Ensures `<home>/token` exists and returns its contents.
///
/// Never overwrites an existing token. Creates `home` if needed. The file is
/// created with `O_CREAT | O_EXCL` (Unix: `create_new` plus mode 0600 set at
/// creation, not after), which is both atomic against a concurrent daemon
/// start racing to create the same file and immune to a window where the
/// file briefly exists with a looser mode. On `AlreadyExists`, another
/// process (or the user) won the race or the file was already there; read
/// and return its contents.
pub fn ensure_token(home: &Path) -> io::Result<String> {
    std::fs::create_dir_all(home)?;
    let path = home.join("token");

    match create_token_file(&path) {
        Ok(token) => Ok(token),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_existing_token(&path),
        Err(e) => Err(e),
    }
}

/// Creates `path` exclusively (fails if it already exists), writes a fresh
/// random token with mode 0600, and returns it.
#[cfg(unix)]
fn create_token_file(path: &Path) -> io::Result<String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let token = random_token_hex();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())?;
    Ok(token)
}

/// Non-Unix fallback: no POSIX mode to set at creation; still exclusive-create.
#[cfg(not(unix))]
fn create_token_file(path: &Path) -> io::Result<String> {
    use std::io::Write;
    let token = random_token_hex();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(token.as_bytes())?;
    Ok(token)
}

/// Reads an existing token file, trimming whitespace.
/// Returns an error if the file is empty: a zero-byte token file is corrupt,
/// not a valid "no token yet" state, since `create_token_file` never writes
/// an empty token.
fn read_existing_token(path: &Path) -> io::Result<String> {
    let contents = std::fs::read_to_string(path)?;
    let trimmed = contents.trim().to_owned();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("token file {} is empty", path.display()),
        ));
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_token_hex_is_64_lowercase_hex_chars() {
        let token = random_token_hex();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn random_token_hex_differs_across_calls() {
        assert_ne!(random_token_hex(), random_token_hex());
    }

    #[test]
    fn constant_time_eq_matches_equal_strings() {
        assert!(constant_time_eq("abc123", "abc123"));
    }

    #[test]
    fn constant_time_eq_rejects_different_strings_of_the_same_length() {
        assert!(!constant_time_eq("abc123", "abc124"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
    }

    #[test]
    fn ensure_token_creates_a_token_file_that_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let token = ensure_token(tmp.path()).expect("ensure_token");
        assert_eq!(token.len(), TOKEN_BYTES * 2);

        let again = ensure_token(tmp.path()).expect("ensure_token again");
        assert_eq!(token, again, "a second call must never overwrite the token");
    }

    #[test]
    fn ensure_token_file_is_mode_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        ensure_token(tmp.path()).expect("ensure_token");
        let meta = std::fs::metadata(tmp.path().join("token")).expect("stat token file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
        assert!(meta.len() > 0);
    }

    #[test]
    fn ensure_token_never_overwrites_a_pre_existing_token() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("token"), "preset-token-value").expect("preset token");
        let token = ensure_token(tmp.path()).expect("ensure_token");
        assert_eq!(token, "preset-token-value");
    }
}
