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
//! A refresh writes the plist, then the binary, then signs the bundle
//! (`codesign`, then `lsregister`). The id must differ on `CONFIRMATIONS`
//! polls in a row before the app relaunches. Those polls only count while the
//! refresh is finished (`refresh_settled`): the bundle binary and the code
//! signature are at least as new as the plist. A relaunch tied to a timer
//! alone could start a bundle with the new plist and the old binary, or one
//! that is still being signed.
//!
//! No `tray-icon`/`tao`/`wry` dependency: `mod.rs`'s `maybe_relaunch_for_upgrade`
//! is the only caller, and the only place this ever actually relaunches
//! anything.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// Longest the app waits for the code signature to catch up with the plist
/// when the binary is already written. A failed `codesign` (the refresh only
/// warns) leaves an older `CodeResources` for good: after this the app goes
/// on without it.
pub const SIGNATURE_GRACE: Duration = Duration::from_secs(30);

/// The modification times that tell whether a bundle refresh has finished.
/// `None`: the file is missing or unreadable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BundleStamps {
    /// `Contents/Info.plist`: written first by a refresh.
    pub plist: Option<SystemTime>,
    /// `Contents/MacOS/turbofig`: written after the plist.
    pub binary: Option<SystemTime>,
    /// `Contents/_CodeSignature/CodeResources`: written by `codesign`, after
    /// the binary.
    pub signature: Option<SystemTime>,
}

/// The stamps of the bundle that `exe` (`<X>.app/Contents/MacOS/<name>`) sits in.
pub fn bundle_stamps(exe: &Path) -> BundleStamps {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let Some(contents) = exe.parent().and_then(Path::parent) else {
        return BundleStamps::default();
    };
    BundleStamps {
        plist: modified(&contents.join("Info.plist")),
        binary: modified(exe),
        signature: modified(&contents.join("_CodeSignature").join("CodeResources")),
    }
}

/// True when a bundle refresh has finished writing: the binary is at least as
/// new as the plist, and so is the code signature. A signature that stays
/// older than the plist (a failed `codesign`) counts after `SIGNATURE_GRACE`
/// since the plist was written. Missing stamps never count.
pub fn refresh_settled(stamps: &BundleStamps, now: SystemTime) -> bool {
    let (Some(plist), Some(binary)) = (stamps.plist, stamps.binary) else {
        return false;
    };
    if binary < plist {
        return false;
    }
    match stamps.signature {
        Some(sig) if sig >= plist => true,
        _ => now
            .duration_since(plist)
            .is_ok_and(|age| age >= SIGNATURE_GRACE),
    }
}

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

    /// Records one poll of a finished refresh. `on_disk` is the id the
    /// bundle's `Info.plist` carries now. Returns true when the app must
    /// relaunch.
    #[cfg(test)]
    pub fn observe(&mut self, on_disk: Option<&str>) -> bool {
        self.observe_settled(on_disk, true)
    }

    /// `observe` that counts a differing id only while `settled` (the refresh
    /// that wrote it has finished, `refresh_settled`). An unsettled poll
    /// restarts the count: the confirmations must all see a finished bundle.
    pub fn observe_settled(&mut self, on_disk: Option<&str>, settled: bool) -> bool {
        if settled && crate::upgrade::app_should_relaunch(self.launched_with.as_deref(), on_disk) {
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

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + secs)
    }

    fn stamps(plist: u64, binary: u64, signature: Option<u64>) -> BundleStamps {
        BundleStamps {
            plist: Some(at(plist)),
            binary: Some(at(binary)),
            signature: signature.map(at),
        }
    }

    #[test]
    fn a_refresh_is_settled_only_when_binary_and_signature_are_not_older_than_the_plist() {
        let now = at(10);
        // Plist just written, binary and signature still the old ones.
        assert!(!refresh_settled(&stamps(8, 1, Some(1)), now));
        // Binary written, signing not done yet.
        assert!(!refresh_settled(&stamps(8, 9, Some(1)), now));
        // Everything written after the plist (equal counts).
        assert!(refresh_settled(&stamps(8, 9, Some(9)), now));
        assert!(refresh_settled(&stamps(8, 8, Some(8)), now));
        // Missing files never count.
        assert!(!refresh_settled(&BundleStamps::default(), now));
        let no_binary = BundleStamps {
            binary: None,
            ..stamps(8, 9, Some(9))
        };
        assert!(!refresh_settled(&no_binary, now));
    }

    #[test]
    fn a_failed_codesign_settles_after_the_grace_but_a_stale_binary_never_does() {
        let old_sig = stamps(8, 9, Some(1));
        assert!(!refresh_settled(&old_sig, at(8 + 29)));
        assert!(refresh_settled(&old_sig, at(8 + 30)));
        // No signature at all behaves the same.
        assert!(refresh_settled(&stamps(8, 9, None), at(8 + 30)));
        // The binary older than the plist is never settled, however late.
        assert!(!refresh_settled(&stamps(8, 1, Some(9)), at(8 + 3600)));
    }

    #[test]
    fn unsettled_polls_never_count_and_restart_the_count() {
        let mut w = RelaunchWatch::new(Some("aa-1".into()));
        for _ in 0..10 {
            assert!(!w.observe_settled(Some("bb-2"), false));
        }
        assert!(!w.observe_settled(Some("bb-2"), true), "first settled poll");
        assert!(!w.observe_settled(Some("bb-2"), false), "count restarts");
        assert!(!w.observe_settled(Some("bb-2"), true));
        assert!(w.observe_settled(Some("bb-2"), true));
    }

    #[test]
    fn bundle_stamps_read_the_three_files_of_the_bundle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contents = dir.path().join("turbofig.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::create_dir_all(contents.join("_CodeSignature")).unwrap();
        let exe = contents.join("MacOS/turbofig");
        std::fs::write(contents.join("Info.plist"), "p").unwrap();
        std::fs::write(&exe, "b").unwrap();
        let set = |p: &Path, t: SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        set(&contents.join("Info.plist"), at(5));
        set(&exe, at(6));
        let before = bundle_stamps(&exe);
        assert_eq!(before.plist, Some(at(5)));
        assert_eq!(before.binary, Some(at(6)));
        assert_eq!(before.signature, None);
        let sig = contents.join("_CodeSignature/CodeResources");
        std::fs::write(&sig, "s").unwrap();
        set(&sig, at(7));
        assert_eq!(bundle_stamps(&exe).signature, Some(at(7)));
        assert!(refresh_settled(&bundle_stamps(&exe), at(8)));
    }

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
