//! Mutual proof of the pairing token.
//!
//! Any local account can bind a free port above 1024. If the daemon is down,
//! another account can listen on 18846/18847 and pose as the daemon. A client
//! that sends the bearer token to that listener hands the token over.
//!
//! So a client proves the listener first. It sends a random nonce. The real
//! daemon answers with `HMAC-SHA256(token, label + nonce)`. Only the holder of
//! the token can compute that value, and the value does not reveal the token.
//! The client sends the token, or its own proof, only after the check passes.
//!
//! Each use has its own label, so a proof made for one step can never be
//! replayed in another step.

use sha2::{Digest, Sha256};

use crate::token::constant_time_eq;

/// Request header that carries the client's nonce.
pub(crate) const NONCE_HEADER: &str = "x-turbofig-nonce";
/// Response header that carries the daemon's proof for that nonce.
pub(crate) const PROOF_HEADER: &str = "x-turbofig-proof";

/// Label for the HTTP daemon proof (`X-Turbofig-Proof`).
pub(crate) const LABEL_HTTP_DAEMON: &str = "turbofig-http-daemon:";
/// Label for the daemon's proof in the WebSocket `CHALLENGE` frame.
pub(crate) const LABEL_WS_DAEMON: &str = "turbofig-ws-daemon:";
/// Label for the plugin's proof in the WebSocket `AUTH` frame.
pub(crate) const LABEL_WS_PLUGIN: &str = "turbofig-ws-plugin:";

const BLOCK: usize = 64;

/// HMAC-SHA256 (RFC 2104) on top of the `sha2` crate.
pub(crate) fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// The proof for one step: lowercase hex of `HMAC-SHA256(token, label + nonce)`.
pub(crate) fn proof(token: &str, label: &str, nonce: &str) -> String {
    let mut msg = Vec::with_capacity(label.len() + nonce.len());
    msg.extend_from_slice(label.as_bytes());
    msg.extend_from_slice(nonce.as_bytes());
    hmac_sha256(token.as_bytes(), &msg)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// True when `candidate` is the proof for this token, label and nonce.
/// The compare runs in constant time.
pub(crate) fn verify(token: &str, label: &str, nonce: &str, candidate: &str) -> bool {
    constant_time_eq(&proof(token, label, nonce), candidate)
}

/// A nonce is 32 to 128 hex characters. Anything else is refused, so a client
/// cannot make the daemon sign an arbitrary string.
pub(crate) fn valid_nonce(nonce: &str) -> bool {
    (32..=128).contains(&nonce.len()) && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A fresh 128-bit nonce from the OS CSPRNG, as 32 lowercase hex characters.
pub(crate) fn new_nonce() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG (getrandom) must be available");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NONCE: &str = "00112233445566778899aabbccddeeff";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn hmac_matches_rfc_4231_test_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231_test_case_6_long_key() {
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    /// Shared cross-check vectors. The plugin test suite pins the same values,
    /// so the Rust and TypeScript sides can never drift apart.
    #[test]
    fn proofs_match_the_shared_cross_check_vectors() {
        assert_eq!(
            proof(TOKEN, LABEL_HTTP_DAEMON, NONCE),
            "aa58167f217dd78d0b1c2056735f63935ce9c9249175603ce936f19340be7eaa"
        );
        assert_eq!(
            proof(TOKEN, LABEL_WS_DAEMON, NONCE),
            "d70d29b0eb2b2eb28afaed71877cec191003d9b684d8fa66bd471581872e79fe"
        );
        assert_eq!(
            proof(TOKEN, LABEL_WS_PLUGIN, NONCE),
            "ccee347e4ca98ab0341bfb3b391044a3c10cb9fd3b7a0763709abab5042e6fa3"
        );
    }

    #[test]
    fn verify_accepts_the_right_proof_and_rejects_others() {
        let good = proof(TOKEN, LABEL_HTTP_DAEMON, NONCE);
        assert!(verify(TOKEN, LABEL_HTTP_DAEMON, NONCE, &good));
        assert!(!verify(TOKEN, LABEL_WS_DAEMON, NONCE, &good));
        assert!(!verify(
            "b".repeat(64).as_str(),
            LABEL_HTTP_DAEMON,
            NONCE,
            &good
        ));
        assert!(!verify(TOKEN, LABEL_HTTP_DAEMON, NONCE, ""));
    }

    #[test]
    fn valid_nonce_accepts_32_to_128_hex_chars_only() {
        assert!(valid_nonce(NONCE));
        assert!(valid_nonce(&"f".repeat(128)));
        assert!(!valid_nonce(&"f".repeat(31)));
        assert!(!valid_nonce(&"f".repeat(129)));
        assert!(!valid_nonce("zz112233445566778899aabbccddeeff"));
        assert!(!valid_nonce(""));
    }

    #[test]
    fn new_nonce_is_valid_and_unique() {
        let a = new_nonce();
        let b = new_nonce();
        assert!(valid_nonce(&a));
        assert_ne!(a, b);
    }
}
