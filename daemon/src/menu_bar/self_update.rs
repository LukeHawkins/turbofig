//! The app's self-update decision: when the daemon (refreshed by `brew
//! upgrade` on its next start, see `app_bundle::app_bundle_outdated`)
//! reports a newer version than this running app binary's own
//! `CARGO_PKG_VERSION`, the app relaunches itself once, then exits, so the
//! next launch picks up the refreshed bundle. Guarded against a relaunch
//! loop by a 1-line state file in the home directory: a given daemon
//! version only ever triggers 1 relaunch.
//!
//! No `tray-icon`/`tao`/`wry` dependency: `mod.rs`'s `maybe_relaunch_for_upgrade`
//! is the only caller, and the only place this ever actually relaunches
//! anything.

use std::path::Path;

/// The state file recording the last daemon version this app already
/// relaunched itself for, directly under the home directory (alongside
/// `token`, `plugin-seen`, etc.).
const STATE_FILE: &str = "app-relaunched-for";

/// True when the app should relaunch itself: the daemon's version is
/// strictly newer than the app's own (by semver), and the app has not
/// already relaunched for this exact daemon version before (`last_relaunched_for`,
/// from `read_last_relaunched_version`).
///
/// False for an unparseable version on either side: a version string this
/// crate itself produces (`env!("CARGO_PKG_VERSION")`, or `/health`'s
/// `version`, the same `CARGO_PKG_VERSION`-derived value) should always
/// parse, so a failure here means something is unexpectedly wrong, and
/// doing nothing is the safe default, not relaunching on a guess.
pub fn should_relaunch_for_upgrade(
    app_version: &str,
    daemon_version: &str,
    last_relaunched_for: Option<&str>,
) -> bool {
    let Ok(app) = semver::Version::parse(app_version) else {
        return false;
    };
    let Ok(daemon) = semver::Version::parse(daemon_version) else {
        return false;
    };
    if daemon <= app {
        return false;
    }
    last_relaunched_for != Some(daemon_version)
}

/// Reads the last daemon version this app already relaunched for, if any.
pub fn read_last_relaunched_version(home: &Path) -> Option<String> {
    std::fs::read_to_string(home.join(STATE_FILE))
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Records `daemon_version` as the last one this app relaunched for, so a
/// later health poll for the same version never triggers a second
/// relaunch (see `should_relaunch_for_upgrade`).
pub fn write_last_relaunched_version(home: &Path, daemon_version: &str) -> std::io::Result<()> {
    std::fs::write(home.join(STATE_FILE), daemon_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunches_when_the_daemon_is_newer_and_never_relaunched_before() {
        assert!(should_relaunch_for_upgrade("1.2.0", "1.3.0", None));
    }

    #[test]
    fn does_not_relaunch_when_versions_match() {
        assert!(!should_relaunch_for_upgrade("1.2.0", "1.2.0", None));
    }

    #[test]
    fn does_not_relaunch_when_the_app_is_newer_than_the_daemon() {
        assert!(!should_relaunch_for_upgrade("1.3.0", "1.2.0", None));
    }

    #[test]
    fn does_not_relaunch_twice_for_the_same_daemon_version() {
        assert!(!should_relaunch_for_upgrade(
            "1.2.0",
            "1.3.0",
            Some("1.3.0")
        ));
    }

    #[test]
    fn relaunches_again_for_a_further_newer_daemon_version() {
        assert!(should_relaunch_for_upgrade("1.2.0", "1.4.0", Some("1.3.0")));
    }

    #[test]
    fn an_unparseable_app_version_never_relaunches() {
        assert!(!should_relaunch_for_upgrade("not-a-version", "1.3.0", None));
    }

    #[test]
    fn an_unparseable_daemon_version_never_relaunches() {
        assert!(!should_relaunch_for_upgrade("1.2.0", "not-a-version", None));
    }

    #[test]
    fn read_last_relaunched_version_is_none_when_the_state_file_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_last_relaunched_version(dir.path()), None);
    }

    #[test]
    fn write_then_read_round_trips_the_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_last_relaunched_version(dir.path(), "1.3.0").expect("write");
        assert_eq!(
            read_last_relaunched_version(dir.path()),
            Some("1.3.0".to_owned())
        );
    }
}
