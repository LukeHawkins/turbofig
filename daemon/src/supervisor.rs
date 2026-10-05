//! Supervised-restart decision logic for `TURBOFIG_SUPERVISED=1`.
//!
//! `main.rs` runs a 30s-interval loop that resolves the stable binary path
//! (`launchd::stable_binary_path`) and compares its canonical target against
//! the one captured at startup; a change means `brew upgrade` replaced the
//! `Cellar` version the stable path points at. The pure decision
//! (`upgrade_detected`) and the drain wait (`wait_for_drain`) are kept here,
//! separate from the real clock and path resolver, so both are unit
//! testable without a real 30s wait or a real filesystem symlink.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The file that `stable` points at right now. On a Homebrew install,
/// `stable` is the `<prefix>/bin/turbofig` symlink, and its target is the
/// versioned `Cellar` binary that `brew upgrade` swaps. Falls back to
/// `stable` itself when it cannot be resolved (for example, mid-upgrade).
pub fn installed_target(stable: &Path) -> PathBuf {
    stable
        .canonicalize()
        .unwrap_or_else(|_| stable.to_path_buf())
}

/// Returns true when `current` differs from `baseline`, i.e. the stable
/// binary path now resolves somewhere else than it did at daemon startup.
pub fn upgrade_detected(baseline: &Path, current: &Path) -> bool {
    baseline != current
}

/// Polls `jobs_in_flight` (number of in-flight jobs) until it reaches zero or
/// `max_wait` elapses, sleeping `poll_interval` between checks. Returns true
/// if it drained to zero, false if it timed out while jobs were still
/// running. The caller exits either way (code 0): launchd's `KeepAlive`
/// restarts a timed-out drain's remaining jobs are lost, same as any other
/// `KeepAlive` restart.
pub async fn wait_for_drain(
    jobs_in_flight: impl Fn() -> usize,
    max_wait: Duration,
    poll_interval: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + max_wait;
    loop {
        if jobs_in_flight() == 0 {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn upgrade_detected_is_false_for_an_identical_path() {
        let p = PathBuf::from("/opt/homebrew/bin/turbofig");
        assert!(!upgrade_detected(&p, &p.clone()));
    }

    #[test]
    fn upgrade_detected_is_true_for_a_different_path() {
        let baseline = PathBuf::from("/opt/homebrew/bin/turbofig");
        let current = PathBuf::from("/opt/homebrew/Cellar/turbofig/1.2.4/bin/turbofig");
        assert!(upgrade_detected(&baseline, &current));
    }

    /// The wiring that the two tests above do not cover: the stable symlink
    /// path stays the same across a `brew upgrade`, so only its resolved
    /// target can reveal the upgrade.
    #[cfg(unix)]
    #[test]
    fn installed_target_changes_when_the_stable_symlink_is_repointed() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cellar = tmp.path().join("Cellar/turbofig");
        let bin_dir = tmp.path().join("bin");
        for version in ["0.1.0", "0.2.0"] {
            let dir = cellar.join(version).join("bin");
            std::fs::create_dir_all(&dir).expect("cellar dir");
            std::fs::write(dir.join("turbofig"), b"").expect("binary");
        }
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let stable = bin_dir.join("turbofig");

        std::os::unix::fs::symlink(cellar.join("0.1.0/bin/turbofig"), &stable).expect("link");
        let baseline = installed_target(&stable);
        assert!(!upgrade_detected(&baseline, &installed_target(&stable)));

        std::fs::remove_file(&stable).expect("unlink");
        std::os::unix::fs::symlink(cellar.join("0.2.0/bin/turbofig"), &stable).expect("relink");
        assert!(upgrade_detected(&baseline, &installed_target(&stable)));
    }

    #[tokio::test]
    async fn wait_for_drain_returns_true_immediately_when_already_at_zero() {
        let drained =
            wait_for_drain(|| 0, Duration::from_millis(50), Duration::from_millis(5)).await;
        assert!(drained);
    }

    #[tokio::test]
    async fn wait_for_drain_returns_true_once_the_count_reaches_zero() {
        let remaining = Cell::new(3u32);
        let drained = wait_for_drain(
            || {
                let n = remaining.get();
                if n > 0 {
                    remaining.set(n - 1);
                }
                n as usize
            },
            Duration::from_millis(200),
            Duration::from_millis(5),
        )
        .await;
        assert!(drained);
    }

    #[tokio::test]
    async fn wait_for_drain_times_out_when_jobs_never_finish() {
        let drained =
            wait_for_drain(|| 1, Duration::from_millis(20), Duration::from_millis(5)).await;
        assert!(!drained);
    }
}
