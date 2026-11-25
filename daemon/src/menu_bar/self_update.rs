//! The app's self-update decision. `brew upgrade` changes the build of the
//! binary a refresh copies into `turbofig.app`, and the refresh stamps that
//! build's id into the bundle's `Info.plist` (`TurbofigBuildId`). The running
//! app remembers the id it launched with. When the id on disk differs from
//! it, the app relaunches itself as a new instance (`open -n`) and exits.
//!
//! The id is compared, not the version: this also catches a rebuild with the
//! same version, a formula revision, and a downgrade. There is no state file
//! and no loop: the new instance launches with the id that is on disk, so it
//! sees no difference.
//!
//! A refresh writes the plist, then the binary, then signs the bundle. The id
//! must therefore differ on `CONFIRMATIONS` polls in a row before the app
//! relaunches, so it never starts a bundle that is half written.
//!
//! No `tray-icon`/`tao`/`wry` dependency: `mod.rs`'s `maybe_relaunch_for_upgrade`
//! is the only caller, and the only place this ever actually relaunches
//! anything.

/// Consecutive polls that must see a different id before the app relaunches.
/// The health poll runs every 2 s, so this is a settle time of about 2 s.
pub const CONFIRMATIONS: u32 = 2;

/// Remembers the build id this app launched with and counts consecutive
/// polls that saw another one on disk.
#[derive(Debug)]
pub struct RelaunchWatch {
    launched_with: Option<String>,
    differing: u32,
}

impl RelaunchWatch {
    pub fn new(launched_with: Option<String>) -> Self {
        Self {
            launched_with,
            differing: 0,
        }
    }

    /// Records one poll. `on_disk` is the id the bundle's `Info.plist`
    /// carries now. Returns true when the app must relaunch.
    pub fn observe(&mut self, on_disk: Option<&str>) -> bool {
        if crate::upgrade::app_should_relaunch(self.launched_with.as_deref(), on_disk) {
            self.differing += 1;
        } else {
            self.differing = 0;
        }
        self.differing >= CONFIRMATIONS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_relaunches_while_the_id_on_disk_is_the_launch_id() {
        let mut w = RelaunchWatch::new(Some("aa-1".into()));
        for _ in 0..10 {
            assert!(!w.observe(Some("aa-1")));
        }
    }

    #[test]
    fn relaunches_after_the_id_differs_on_confirmed_polls_in_a_row() {
        let mut w = RelaunchWatch::new(Some("aa-1".into()));
        assert!(!w.observe(Some("bb-2")), "one poll is not enough");
        assert!(w.observe(Some("bb-2")));
    }

    #[test]
    fn a_blip_that_goes_back_resets_the_count() {
        let mut w = RelaunchWatch::new(Some("aa-1".into()));
        assert!(!w.observe(Some("bb-2")));
        assert!(!w.observe(Some("aa-1")));
        assert!(!w.observe(Some("bb-2")), "the count started over");
        assert!(w.observe(Some("bb-2")));
    }

    #[test]
    fn an_unreadable_plist_or_an_unknown_launch_id_never_relaunches() {
        let mut w = RelaunchWatch::new(Some("aa-1".into()));
        for _ in 0..5 {
            assert!(!w.observe(None));
        }
        let mut unknown = RelaunchWatch::new(None);
        for _ in 0..5 {
            assert!(!unknown.observe(Some("bb-2")));
        }
    }

    #[test]
    fn a_same_version_rebuild_and_a_downgrade_both_relaunch() {
        // The ids are opaque: any difference counts, whatever the version.
        for disk in ["bb-2", "00-0"] {
            let mut w = RelaunchWatch::new(Some("aa-1".into()));
            w.observe(Some(disk));
            assert!(w.observe(Some(disk)));
        }
    }
}
