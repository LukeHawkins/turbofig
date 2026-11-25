//! Pure decisions for the menu-bar app's own upkeep: when the health poller
//! restarts a dead daemon, how "Start at login" changes without touching the
//! running app or daemon, and the text of the start-failure alert. No tray,
//! window, `launchctl`, or real home is involved, so all of it is unit-tested.

use std::io;
use std::path::{Path, PathBuf};

/// Consecutive failed health checks (daemon unreachable) before the poller
/// starts the daemon again. At the 2 second poll interval this is about 6s.
pub const RESPAWN_AFTER_FAILURES: u32 = 3;

/// Counts consecutive "daemon unreachable" ticks.
#[derive(Debug, Default)]
pub struct RespawnTracker {
    failures: u32,
}

impl RespawnTracker {
    /// Records one health check. `unreachable` is true only when nothing
    /// listens on the port (a foreign or unverified listener is not a dead
    /// daemon). Returns true when the poller must start the daemon now.
    /// Never returns true while `quitting`. The count restarts after a
    /// respawn request, so a daemon that keeps dying is retried every
    /// `RESPAWN_AFTER_FAILURES` ticks, not on every tick.
    pub fn observe(&mut self, unreachable: bool, quitting: bool) -> bool {
        if quitting || !unreachable {
            self.failures = 0;
            return false;
        }
        self.failures += 1;
        if self.failures >= RESPAWN_AFTER_FAILURES {
            self.failures = 0;
            return true;
        }
        false
    }
}

/// True when the health poller must not start the daemon now: Quit is under
/// way, or the user stopped the daemon on purpose (`turbofig stop` leaves the
/// `stopped-by-user` marker in `home`). Without the marker check the app
/// would start the daemon again about 6 s after a stop, which makes `stop`
/// meaningless and breaks the documented token rotation (stop, delete the
/// token, start).
pub fn respawn_held(home: &Path, quitting: bool) -> bool {
    quitting || crate::secure_fs::stop_marker_present(home)
}

/// "Start at login" is On when either the app or the headless plist exists.
pub fn login_item_on(launch_agents_dir: &Path) -> bool {
    crate::cli::autostart_plist_exists(launch_agents_dir)
}

/// Turns "Start at login" On from the running app: writes the app plist and
/// removes the headless plist. It never runs `launchctl`: `bootstrap` would
/// start a second app (`RunAtLoad`), and `bootout` of the headless service
/// would kill the running daemon. launchd reads the new plist at next login.
pub fn enable_login_item(
    launch_agents_dir: &Path,
    bundle_exe: &Path,
    home: &Path,
    extra_env: &[(String, String)],
) -> io::Result<PathBuf> {
    if !bundle_exe.exists() {
        return Err(io::Error::other(format!(
            "{} is not installed; run `turbofig autostart on` once",
            bundle_exe.display()
        )));
    }
    std::fs::create_dir_all(launch_agents_dir)?;
    let plist = launch_agents_dir.join(crate::launchd::app_plist_file_name());
    std::fs::write(
        &plist,
        crate::launchd::app_plist_contents(bundle_exe, &home.join("daemon.log"), extra_env),
    )?;
    match std::fs::remove_file(launch_agents_dir.join(crate::launchd::plist_file_name())) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(plist)
}

/// Turns "Start at login" Off from the running app: removes both plists.
/// No `launchctl bootout`: the running app (which may BE the app service)
/// and the running daemon (which may be the headless service) stay up.
pub fn disable_login_item(launch_agents_dir: &Path) -> io::Result<()> {
    for name in [
        crate::launchd::plist_file_name(),
        crate::launchd::app_plist_file_name(),
    ] {
        match std::fs::remove_file(launch_agents_dir.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The AppleScript that shows the start-failure alert. Quotes and
/// backslashes in `message` are escaped, so it cannot break out of the string.
pub fn failure_alert_script(message: &str, log_path: &Path) -> String {
    let text = format!("{message}\n\nLog: {}", log_path.display());
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "display alert \"turbofig could not start\" message \"{escaped}\" as critical buttons {{\"OK\"}} default button \"OK\""
    )
}

/// Prints `message` to stderr and shows a native alert (via `osascript`) with
/// the log path, so a failure at login is not invisible. Best effort.
pub fn show_failure_alert(message: &str, home: &Path) {
    eprintln!("turbofig: {message}");
    #[cfg(target_os = "macos")]
    {
        #[cfg(debug_assertions)]
        if std::env::var("TURBOFIG_TEST_NO_ALERT").is_ok() {
            return;
        }
        let script = failure_alert_script(message, &home.join("daemon.log"));
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = home;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respawns_after_three_failures_in_a_row() {
        let mut t = RespawnTracker::default();
        assert!(!t.observe(true, false));
        assert!(!t.observe(true, false));
        assert!(t.observe(true, false));
        // The count restarted: not again on the next tick.
        assert!(!t.observe(true, false));
    }

    #[test]
    fn the_stop_marker_holds_the_respawn_until_it_is_cleared() {
        let home = tempfile::tempdir().expect("tempdir");
        assert!(!respawn_held(home.path(), false));
        assert!(respawn_held(home.path(), true));
        crate::secure_fs::write_stop_marker(home.path()).unwrap();
        assert!(respawn_held(home.path(), false));
        // The tracker never asks for a respawn while the marker holds it.
        let mut t = RespawnTracker::default();
        for _ in 0..10 {
            let held = respawn_held(home.path(), false);
            assert!(!t.observe(true, held));
        }
        // "Start turbofig" clears the marker: the poller works again.
        crate::secure_fs::clear_stop_marker(home.path());
        let mut t = RespawnTracker::default();
        let mut respawned = false;
        for _ in 0..RESPAWN_AFTER_FAILURES {
            respawned = t.observe(true, respawn_held(home.path(), false));
        }
        assert!(respawned);
    }

    #[test]
    fn a_healthy_tick_resets_the_count() {
        let mut t = RespawnTracker::default();
        assert!(!t.observe(true, false));
        assert!(!t.observe(true, false));
        assert!(!t.observe(false, false));
        assert!(!t.observe(true, false));
        assert!(!t.observe(true, false));
        assert!(t.observe(true, false));
    }

    #[test]
    fn never_respawns_while_quitting() {
        let mut t = RespawnTracker::default();
        for _ in 0..10 {
            assert!(!t.observe(true, true));
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let d = tempfile::tempdir().expect("tempdir");
        let agents = d.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let exe = d.path().join("turbofig");
        std::fs::write(&exe, "x").unwrap();
        let home = d.path().join("home");
        (d, agents, exe, home)
    }

    #[test]
    fn settings_state_is_on_for_either_plist() {
        let (_d, agents, exe, home) = fixture();
        assert!(!login_item_on(&agents));
        std::fs::write(agents.join(crate::launchd::plist_file_name()), "x").unwrap();
        assert!(login_item_on(&agents));
        std::fs::remove_file(agents.join(crate::launchd::plist_file_name())).unwrap();
        enable_login_item(&agents, &exe, &home, &[]).unwrap();
        assert!(login_item_on(&agents));
    }

    #[test]
    fn enable_swaps_the_headless_plist_for_the_app_plist_without_launchctl() {
        let (_d, agents, exe, home) = fixture();
        let headless = agents.join(crate::launchd::plist_file_name());
        std::fs::write(&headless, "x").unwrap();
        let plist = enable_login_item(&agents, &exe, &home, &[]).unwrap();
        assert!(plist.exists());
        assert!(!headless.exists());
        let body = std::fs::read_to_string(plist).unwrap();
        assert!(body.contains(&exe.display().to_string()));
    }

    #[test]
    fn enable_fails_without_the_bundle_executable() {
        let (_d, agents, exe, home) = fixture();
        std::fs::remove_file(&exe).unwrap();
        assert!(enable_login_item(&agents, &exe, &home, &[]).is_err());
        assert!(!login_item_on(&agents));
    }

    #[test]
    fn disable_removes_both_plists_and_is_idempotent() {
        let (_d, agents, exe, home) = fixture();
        enable_login_item(&agents, &exe, &home, &[]).unwrap();
        std::fs::write(agents.join(crate::launchd::plist_file_name()), "x").unwrap();
        disable_login_item(&agents).unwrap();
        assert!(!login_item_on(&agents));
        disable_login_item(&agents).unwrap();
    }

    #[test]
    fn alert_script_escapes_quotes_and_names_the_log() {
        let s = failure_alert_script("bad \"x\" \\ y", Path::new("/h/daemon.log"));
        assert!(s.contains("bad \\\"x\\\" \\\\ y"));
        assert!(s.contains("/h/daemon.log"));
        assert!(s.starts_with("display alert"));
    }
}
