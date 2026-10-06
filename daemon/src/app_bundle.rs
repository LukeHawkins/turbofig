//! Assembles `Turbofig.app`: a macOS app bundle the daemon builds itself on
//! the user's own Mac, so it carries no Gatekeeper quarantine flag and opens
//! with no "unidentified developer" prompt, even though the `turbofig`
//! binary itself is not notarized. macOS-only: a bundle, an `Info.plist`, an
//! `.icns` and ad-hoc code signing are all macOS concepts, so every public
//! item here only exists under `cfg(target_os = "macos")` (see `lib.rs`).
//!
//! Step 1 of the menu-bar app: this module only builds and maintains the
//! bundle. The menu-bar UI (step 2), the About window (step 3), and the
//! app lifecycle: first run opens the app, Start at Login, self-update
//! (step 4a), and docs (step 4b), all live in `menu_bar/`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The app bundle's own identifier, distinct from the daemon's CLI, so
/// `uninstall` can tell a Turbofig.app we wrote from an unrelated app that
/// happens to share the name.
pub const BUNDLE_IDENTIFIER: &str = "eu.lukehawkins.turbofig.app";

/// The placeholder icon: a dark rounded square with "tf" in white, built
/// with macOS's own `sips` (see `daemon/assets/app-icon/README.md`). The
/// owner replaces this with the real brand icon later; `scripts/make-app-icon.sh`
/// regenerates the `.icns` from a new 1024px PNG with no other change needed
/// here.
const APP_ICON_BYTES: &[u8] = include_bytes!("../assets/app-icon/AppIcon.icns");

/// Seam over `codesign`, so a test never shells out to the real binary or
/// touches Gatekeeper state. `install_app_bundle` signs best-effort: a
/// signing failure is logged, never returned as an install failure.
pub trait CodeSigner {
    fn sign(&self, bundle_path: &Path) -> io::Result<()>;
}

/// The real signer: ad-hoc signs (`--sign -`) so the bundle gets a valid
/// signature with no Apple Developer identity, which is enough to satisfy
/// Gatekeeper for a locally built, unquarantined bundle.
pub struct RealCodeSigner;

impl CodeSigner for RealCodeSigner {
    fn sign(&self, bundle_path: &Path) -> io::Result<()> {
        let status = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(bundle_path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("codesign exited with {status}")))
        }
    }
}

/// A signer that does nothing. Used by every test and by nothing else, so a
/// test run never shells out to `codesign` or touches the real bundle's
/// signature.
pub struct NoopCodeSigner;

impl CodeSigner for NoopCodeSigner {
    fn sign(&self, _bundle_path: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// Resolves the directory `install_app_bundle` assembles `Turbofig.app`
/// into: `TURBOFIG_APPLICATIONS_DIR` if set, else the real `~/Applications`.
pub fn applications_dir_from_env() -> PathBuf {
    if let Ok(dir) = std::env::var("TURBOFIG_APPLICATIONS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Applications")
}

/// The real `~/Applications`, with no `TURBOFIG_APPLICATIONS_DIR` override
/// applied. Used only by the debug guard below.
fn real_applications_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join("Applications"))
}

/// True when a debug build may touch the real `~/Applications`. Mirrors
/// `main.rs`'s `debug_real_desktop_allowed` guard for the clipboard and the
/// Figma-desktop opener.
#[cfg(debug_assertions)]
fn debug_real_desktop_allowed() -> bool {
    std::env::var("TURBOFIG_DEV_REAL_DESKTOP").as_deref() == Ok("1")
}

/// Refuses to proceed when `applications_dir` is the real, un-overridden
/// `~/Applications` and this is a debug build without
/// `TURBOFIG_DEV_REAL_DESKTOP=1`. A no-op in a release build (what Homebrew
/// installs): only a `cargo build`/`cargo test` debug build ever hits this
/// guard, so a test run can never write the owner's real Applications
/// folder even if it forgets to set `TURBOFIG_APPLICATIONS_DIR` itself.
#[cfg(debug_assertions)]
fn refuse_real_applications_dir(applications_dir: &Path) -> io::Result<()> {
    if debug_real_desktop_allowed() {
        return Ok(());
    }
    if Some(applications_dir.to_path_buf()) == real_applications_dir() {
        return Err(io::Error::other(
            "refusing to write the real ~/Applications in a debug build; set \
             TURBOFIG_APPLICATIONS_DIR to a test directory, or TURBOFIG_DEV_REAL_DESKTOP=1 \
             to override (never in a test)",
        ));
    }
    Ok(())
}

#[cfg(not(debug_assertions))]
fn refuse_real_applications_dir(_applications_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Builds `Contents/Info.plist`'s full contents, stamping this binary's own
/// crate version into both version keys.
fn info_plist_contents() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Turbofig</string>
	<key>CFBundleDisplayName</key>
	<string>Turbofig</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_IDENTIFIER}</string>
	<key>CFBundleExecutable</key>
	<string>turbofig</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSUIElement</key>
	<true/>
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
"#
    )
}

/// Writes `contents` to `path` atomically: a sibling `.tmp` file (mode
/// `mode` from creation, so an executable is never briefly non-executable),
/// flushed and closed, then renamed over `path`. The same pattern
/// `plugin_files.rs`'s `write_atomic_0600` uses, parametrized on mode.
fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(mode);
        }
        let mut file = opts.open(&tmp_path)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Assembles `<applications_dir>/Turbofig.app`: `Contents/Info.plist`, the
/// placeholder icon, and a byte copy of `own_exe` (symlinks resolved first)
/// at `Contents/MacOS/turbofig`. Ad-hoc signs the finished bundle
/// best-effort (a failure is logged, never returned).
///
/// Refuses to write the real `~/Applications` from a debug build unless
/// `TURBOFIG_DEV_REAL_DESKTOP=1` (see `refuse_real_applications_dir`).
pub fn install_app_bundle(applications_dir: &Path, own_exe: &Path) -> io::Result<PathBuf> {
    install_app_bundle_with_signer(applications_dir, own_exe, &RealCodeSigner)
}

/// The testable half of `install_app_bundle`: takes an explicit `signer` so
/// a test exercises the whole assembly without ever shelling out to the
/// real `codesign`.
pub fn install_app_bundle_with_signer(
    applications_dir: &Path,
    own_exe: &Path,
    signer: &dyn CodeSigner,
) -> io::Result<PathBuf> {
    refuse_real_applications_dir(applications_dir)?;

    let bundle_dir = applications_dir.join("Turbofig.app");
    let contents_dir = bundle_dir.join("Contents");
    let macos_dir = contents_dir.join("MacOS");
    let resources_dir = contents_dir.join("Resources");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(&resources_dir)?;

    write_atomic(
        &contents_dir.join("Info.plist"),
        info_plist_contents().as_bytes(),
        0o644,
    )?;
    write_atomic(&resources_dir.join("AppIcon.icns"), APP_ICON_BYTES, 0o644)?;

    // Resolve symlinks (a Homebrew Cellar binary is reached through the
    // stable <prefix>/bin/turbofig symlink) and copy the real bytes. This
    // write happens last and atomically: a crash between the plist/icon
    // write above and this one leaves a bundle with no executable at all,
    // which macOS simply refuses to launch, never a half-written binary.
    let resolved_exe = own_exe.canonicalize()?;
    let exe_bytes = std::fs::read(&resolved_exe)?;
    write_atomic(&macos_dir.join("turbofig"), &exe_bytes, 0o755)?;

    if let Err(e) = signer.sign(&bundle_dir) {
        eprintln!(
            "turbofig: warning: could not ad-hoc sign {}: {e}",
            bundle_dir.display()
        );
    }

    Ok(bundle_dir)
}

/// Extracts the string value following `<key>{key}</key>` in a plist's XML.
/// Only ever reads a plist this module itself wrote, so this narrow scan
/// (not a general XML/plist parser) is enough.
fn extract_plist_string(xml: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let after_key = &xml[xml.find(&key_tag)? + key_tag.len()..];
    let value_start = after_key.find("<string>")? + "<string>".len();
    let value_end = after_key[value_start..].find("</string>")?;
    Some(after_key[value_start..value_start + value_end].to_owned())
}

/// True when `<applications_dir>/Turbofig.app` exists but its
/// `CFBundleShortVersionString` differs from this binary's own version, or
/// its executable is missing. False when the bundle does not exist at all:
/// step 4 decides whether to create one, never the daemon's own startup
/// check (see `main.rs`'s `run_daemon`).
pub fn app_bundle_outdated(applications_dir: &Path) -> bool {
    let bundle_dir = applications_dir.join("Turbofig.app");
    let Ok(plist_contents) = std::fs::read_to_string(bundle_dir.join("Contents/Info.plist")) else {
        return false;
    };
    let exe_missing = !bundle_dir.join("Contents/MacOS/turbofig").exists();
    let version_mismatch = extract_plist_string(&plist_contents, "CFBundleShortVersionString")
        .as_deref()
        != Some(env!("CARGO_PKG_VERSION"));
    exe_missing || version_mismatch
}

/// The bundle directory itself: `<applications_dir>/Turbofig.app`.
pub fn app_bundle_path(applications_dir: &Path) -> PathBuf {
    applications_dir.join("Turbofig.app")
}

/// The bundle's own executable: `<applications_dir>/Turbofig.app/Contents/MacOS/turbofig`.
/// Used by the app LaunchAgent (`cli::run_autostart_on_app`) to pin its
/// `ProgramArguments` at the bundle itself, not the Homebrew binary.
pub fn app_bundle_executable_path(applications_dir: &Path) -> PathBuf {
    app_bundle_path(applications_dir).join("Contents/MacOS/turbofig")
}

/// True when the running binary's own canonical path is inside a `.app`
/// bundle's `Contents/MacOS/`: i.e. this process was launched by opening
/// `Turbofig.app`, not by a CLI invocation. Step 2 uses this to switch into
/// the menu-bar app; for now `main.rs` only uses it to pick the stub
/// `run_menu_bar_app`.
pub fn running_inside_app_bundle() -> bool {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map(|p| {
            p.to_string_lossy()
                .ends_with(".app/Contents/MacOS/turbofig")
        })
        .unwrap_or(false)
}

/// Removes `<applications_dir>/Turbofig.app`, but only if its `Info.plist`
/// carries our own `BUNDLE_IDENTIFIER`: a foreign `Turbofig.app` (an
/// unrelated app that happens to share the name) is left untouched. Returns
/// `Ok(true)` when a bundle was actually removed, `Ok(false)` when none was
/// there or it belonged to someone else: both are success, not an error.
pub fn remove_turbofig_app_bundle(applications_dir: &Path) -> io::Result<bool> {
    let bundle_dir = applications_dir.join("Turbofig.app");
    let Ok(plist_contents) = std::fs::read_to_string(bundle_dir.join("Contents/Info.plist")) else {
        return Ok(false);
    };
    let owns_it = extract_plist_string(&plist_contents, "CFBundleIdentifier").as_deref()
        == Some(BUNDLE_IDENTIFIER);
    if !owns_it {
        return Ok(false);
    }
    std::fs::remove_dir_all(&bundle_dir)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_bundle_path_joins_turbofig_app() {
        assert_eq!(
            app_bundle_path(Path::new("/Users/dev/Applications")),
            PathBuf::from("/Users/dev/Applications/Turbofig.app")
        );
    }

    #[test]
    fn app_bundle_executable_path_points_at_contents_macos() {
        assert_eq!(
            app_bundle_executable_path(Path::new("/Users/dev/Applications")),
            PathBuf::from("/Users/dev/Applications/Turbofig.app/Contents/MacOS/turbofig")
        );
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("turbofig-app-bundle-test-{label}-{nanos}"))
    }

    fn run_plutil_lint(path: &Path) {
        let status = std::process::Command::new("plutil")
            .args(["-lint"])
            .arg(path)
            .status()
            .expect("run plutil -lint");
        assert!(status.success(), "Info.plist must pass plutil -lint");
    }

    #[test]
    fn install_app_bundle_assembles_a_valid_bundle() {
        let applications_dir = unique_temp_dir("assemble");
        let own_exe = std::env::current_exe().expect("current_exe");

        let bundle_dir =
            install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
                .expect("install_app_bundle_with_signer");

        assert_eq!(bundle_dir, applications_dir.join("Turbofig.app"));
        let info_plist = bundle_dir.join("Contents/Info.plist");
        assert!(info_plist.exists());
        run_plutil_lint(&info_plist);

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn install_app_bundle_info_plist_carries_the_expected_keys() {
        let applications_dir = unique_temp_dir("keys");
        let own_exe = std::env::current_exe().expect("current_exe");

        let bundle_dir =
            install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
                .expect("install");
        let contents =
            std::fs::read_to_string(bundle_dir.join("Contents/Info.plist")).expect("read plist");

        assert_eq!(
            extract_plist_string(&contents, "CFBundleName").as_deref(),
            Some("Turbofig")
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleDisplayName").as_deref(),
            Some("Turbofig")
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleIdentifier").as_deref(),
            Some(BUNDLE_IDENTIFIER)
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleExecutable").as_deref(),
            Some("turbofig")
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleIconFile").as_deref(),
            Some("AppIcon")
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundlePackageType").as_deref(),
            Some("APPL")
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleShortVersionString").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleVersion").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            extract_plist_string(&contents, "LSMinimumSystemVersion").as_deref(),
            Some("12.0")
        );
        assert!(contents.contains("<key>LSUIElement</key>\n\t<true/>"));
        assert!(contents.contains("<key>NSHighResolutionCapable</key>\n\t<true/>"));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn install_app_bundle_copies_the_binary_executable_and_byte_identical() {
        let applications_dir = unique_temp_dir("exe-copy");
        let own_exe = std::env::current_exe().expect("current_exe");
        let own_exe_bytes = std::fs::read(own_exe.canonicalize().expect("canonicalize"))
            .expect("read own exe bytes");

        let bundle_dir =
            install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
                .expect("install");
        let copied_exe = bundle_dir.join("Contents/MacOS/turbofig");
        let copied_bytes = std::fs::read(&copied_exe).expect("read copied exe");

        assert_eq!(copied_bytes, own_exe_bytes);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&copied_exe)
                .expect("stat copied exe")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755);
        }

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn install_app_bundle_writes_the_icon() {
        let applications_dir = unique_temp_dir("icon");
        let own_exe = std::env::current_exe().expect("current_exe");

        let bundle_dir =
            install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
                .expect("install");

        let icon_path = bundle_dir.join("Contents/Resources/AppIcon.icns");
        assert!(icon_path.exists());
        assert_eq!(
            std::fs::read(&icon_path).expect("read icon"),
            APP_ICON_BYTES
        );

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn app_bundle_outdated_is_false_when_no_bundle_exists() {
        let applications_dir = unique_temp_dir("outdated-missing");
        assert!(!app_bundle_outdated(&applications_dir));
    }

    #[test]
    fn app_bundle_outdated_is_false_right_after_install() {
        let applications_dir = unique_temp_dir("outdated-fresh");
        let own_exe = std::env::current_exe().expect("current_exe");
        install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
            .expect("install");

        assert!(!app_bundle_outdated(&applications_dir));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn app_bundle_outdated_is_true_when_the_version_differs() {
        let applications_dir = unique_temp_dir("outdated-version");
        let own_exe = std::env::current_exe().expect("current_exe");
        let bundle_dir =
            install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
                .expect("install");

        let plist_path = bundle_dir.join("Contents/Info.plist");
        let contents = std::fs::read_to_string(&plist_path).expect("read plist");
        let rewritten = contents.replace(env!("CARGO_PKG_VERSION"), "0.0.1-older");
        std::fs::write(&plist_path, rewritten).expect("rewrite plist with an old version");

        assert!(app_bundle_outdated(&applications_dir));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn app_bundle_outdated_is_true_when_the_executable_is_missing() {
        let applications_dir = unique_temp_dir("outdated-exe-missing");
        let own_exe = std::env::current_exe().expect("current_exe");
        let bundle_dir =
            install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
                .expect("install");

        std::fs::remove_file(bundle_dir.join("Contents/MacOS/turbofig"))
            .expect("remove copied exe");

        assert!(app_bundle_outdated(&applications_dir));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn remove_turbofig_app_bundle_removes_our_own_bundle() {
        let applications_dir = unique_temp_dir("uninstall-own");
        let own_exe = std::env::current_exe().expect("current_exe");
        install_app_bundle_with_signer(&applications_dir, &own_exe, &NoopCodeSigner)
            .expect("install");

        let removed =
            remove_turbofig_app_bundle(&applications_dir).expect("remove_turbofig_app_bundle");

        assert!(removed);
        assert!(!applications_dir.join("Turbofig.app").exists());

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn remove_turbofig_app_bundle_is_a_no_op_when_nothing_is_there() {
        let applications_dir = unique_temp_dir("uninstall-missing");
        let removed =
            remove_turbofig_app_bundle(&applications_dir).expect("remove_turbofig_app_bundle");
        assert!(!removed);
    }

    #[test]
    fn remove_turbofig_app_bundle_never_touches_a_foreign_bundle() {
        let applications_dir = unique_temp_dir("uninstall-foreign");
        let foreign_bundle = applications_dir.join("Turbofig.app");
        let foreign_contents = foreign_bundle.join("Contents");
        std::fs::create_dir_all(&foreign_contents).expect("mkdir foreign bundle");
        std::fs::write(
            foreign_contents.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.example.other</string></dict></plist>",
        )
        .expect("write foreign plist");

        let removed =
            remove_turbofig_app_bundle(&applications_dir).expect("remove_turbofig_app_bundle");

        assert!(!removed, "a foreign Turbofig.app must never be removed");
        assert!(foreign_bundle.exists());

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn install_app_bundle_refuses_the_real_applications_directory() {
        let Some(real_dir) = real_applications_dir() else {
            return; // HOME unset in this environment; nothing to guard
        };
        let own_exe = std::env::current_exe().expect("current_exe");

        // This must fail before touching the filesystem at all: no write,
        // no create_dir_all, nothing under the real ~/Applications.
        let result = install_app_bundle_with_signer(&real_dir, &own_exe, &NoopCodeSigner);

        assert!(
            result.is_err(),
            "a debug build must refuse to install into the real ~/Applications"
        );
        assert!(
            !real_dir.join("Turbofig.app").exists(),
            "the guard must refuse before writing anything"
        );
    }
}
