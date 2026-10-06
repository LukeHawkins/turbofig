//! Assembles `turbofig.app`: a macOS app bundle the daemon builds itself on
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
/// `uninstall` can tell a turbofig.app we wrote from an unrelated app that
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

/// The global `/Applications`: what Finder's own sidebar shows. Writable
/// only by an admin account (`access(W_OK)`); see `dir_is_writable`.
pub fn global_applications_dir() -> PathBuf {
    PathBuf::from("/Applications")
}

/// The per-user `~/Applications`: the fallback for a non-admin account on a
/// managed Mac, where `global_applications_dir` is not writable.
pub fn home_applications_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Applications")
}

/// True when `dir` already holds a `turbofig.app` bundle that is ours (its
/// `Info.plist` carries `BUNDLE_IDENTIFIER`): a foreign app that happens to
/// share the name does not count. Used by the resolver below so an
/// existing install is always found and reused, never duplicated.
pub fn has_our_bundle(dir: &Path) -> bool {
    bundle_identifier_at(dir).as_deref() == Some(BUNDLE_IDENTIFIER)
}

/// Reads `<dir>/turbofig.app`'s own `CFBundleIdentifier`, or `None` if no
/// bundle (or no readable `Info.plist`) is there. Shared by `has_our_bundle`
/// and `remove_turbofig_app_bundle`'s ownership check.
fn bundle_identifier_at(dir: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(dir.join("turbofig.app/Contents/Info.plist")).ok()?;
    extract_plist_string(&plist, "CFBundleIdentifier")
}

/// True when this process can write into `dir` (`access(W_OK)`): true for
/// an admin account against `/Applications`, false for a standard account
/// on a managed Mac. `false` (not an error) when `dir` does not exist yet
/// either: `access` itself reports that as not writable, which is the
/// right answer here (nothing to create it with elevated rights).
pub fn dir_is_writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

/// Resolves the directory `turbofig.app` lives (or should be installed)
/// in: every caller that needs the bundle's location goes through this one
/// function (the outdated check, autostart, uninstall, the app
/// LaunchAgent's `ProgramArguments`, and `set_start_at_login`), so there is
/// one shared answer, never two.
///
/// `TURBOFIG_APPLICATIONS_DIR` wins outright, for tests and for manual
/// overrides. Otherwise: prefers whichever of `global_applications_dir`
/// (`/Applications`, what Finder's sidebar shows) or `home_applications_dir`
/// (`~/Applications`, the non-admin fallback) already holds our own bundle,
/// checked in that order, so an existing install is always found and never
/// duplicated; if neither does, picks `/Applications` when writable (the
/// common case on an unmanaged Mac), else `~/Applications` (a managed Mac
/// with a non-admin account).
pub fn applications_dir_from_env() -> PathBuf {
    if let Ok(dir) = std::env::var("TURBOFIG_APPLICATIONS_DIR") {
        return PathBuf::from(dir);
    }
    let global = global_applications_dir();
    let home = home_applications_dir();
    if has_our_bundle(&global) {
        return global;
    }
    if has_our_bundle(&home) {
        return home;
    }
    if dir_is_writable(&global) {
        global
    } else {
        home
    }
}

/// True when a debug build may touch the real `~/Applications`. Mirrors
/// `main.rs`'s `debug_real_desktop_allowed` guard for the clipboard and the
/// Figma-desktop opener.
#[cfg(debug_assertions)]
fn debug_real_desktop_allowed() -> bool {
    std::env::var("TURBOFIG_DEV_REAL_DESKTOP").as_deref() == Ok("1")
}

/// True when `dir` is one of the 2 real, un-overridden Applications
/// folders (`global_applications_dir`/`home_applications_dir`), with no
/// `TURBOFIG_APPLICATIONS_DIR` substitution applied. Used only by the
/// debug guard below.
#[cfg(debug_assertions)]
fn is_a_real_applications_dir(dir: &Path) -> bool {
    dir == global_applications_dir() || dir == home_applications_dir()
}

/// Refuses to proceed when `applications_dir` is one of the real,
/// un-overridden Applications folders (`/Applications` or
/// `~/Applications`) and this is a debug build without
/// `TURBOFIG_DEV_REAL_DESKTOP=1`. A no-op in a release build (what Homebrew
/// installs): only a `cargo build`/`cargo test` debug build ever hits this
/// guard, so a test run can never write either of the owner's real
/// Applications folders even if it forgets to set
/// `TURBOFIG_APPLICATIONS_DIR` itself.
#[cfg(debug_assertions)]
fn refuse_real_applications_dir(applications_dir: &Path) -> io::Result<()> {
    if debug_real_desktop_allowed() {
        return Ok(());
    }
    if is_a_real_applications_dir(applications_dir) {
        return Err(io::Error::other(
            "refusing to write a real Applications folder (/Applications or ~/Applications) in \
             a debug build; set TURBOFIG_APPLICATIONS_DIR to a test directory, or \
             TURBOFIG_DEV_REAL_DESKTOP=1 to override (never in a test)",
        ));
    }
    Ok(())
}

#[cfg(not(debug_assertions))]
fn refuse_real_applications_dir(_applications_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Builds `Contents/Info.plist`'s full contents, stamping this binary's own
/// crate version into both version keys, plus `build_id` (see
/// `build_id_for_bytes`) into the custom `TurbofigBuildId` key:
/// `app_bundle_outdated` reads it back to detect a rebuild that kept the
/// same `CARGO_PKG_VERSION`. `CFBundleInfoDictionaryVersion` and
/// `LSApplicationCategoryType` are both here because Spotlight/Launchpad
/// indexing (via `lsregister`, see `install_app_bundle_with_signer`) wants
/// them to treat the bundle as a real, categorized app.
fn info_plist_contents(build_id: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>turbofig</string>
	<key>CFBundleDisplayName</key>
	<string>turbofig</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_IDENTIFIER}</string>
	<key>CFBundleExecutable</key>
	<string>turbofig</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>TurbofigBuildId</key>
	<string>{build_id}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
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

/// A fingerprint of a binary's exact bytes: FNV-1a (64-bit) over the whole
/// file, combined with its length. Not cryptographic, and deliberately not
/// `std::hash::DefaultHasher` (its algorithm carries no stability guarantee
/// across Rust versions, so 2 builds of the identical binary on different
/// toolchains could disagree); FNV-1a's definition never changes, so the
/// same bytes always produce the same id. Good enough to answer "is this
/// the same build", which is all `app_bundle_outdated` needs it for: this
/// is not a security boundary.
///
/// Cheaper than a full byte-for-byte compare against the bundle's own
/// installed copy: that would mean reading both the running binary and the
/// installed one in full on every daemon start, where this only ever reads
/// the running binary (the installed copy's id is already sitting in its
/// `Info.plist`, no second read needed).
fn build_id_for_bytes(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}-{}", bytes.len())
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

/// Seam over `lsregister`, so a test never shells out to it or touches the
/// real Spotlight/Launch Services index. `install_app_bundle` registers
/// best-effort: a failure is logged, never returned as an install failure.
pub trait LaunchServicesRegistrar {
    fn register(&self, bundle_path: &Path) -> io::Result<()>;
}

/// The real registrar: re-registers the bundle with Launch Services, so
/// Spotlight and Launchpad find it without the user waiting for (or
/// triggering) a background reindex.
pub struct RealLaunchServicesRegistrar;

/// The `lsregister` tool's fixed path, part of the `LaunchServices`
/// framework shipped with every macOS since well before this crate's
/// `LSMinimumSystemVersion` (12.0).
const LSREGISTER_PATH: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
LaunchServices.framework/Support/lsregister";

impl LaunchServicesRegistrar for RealLaunchServicesRegistrar {
    fn register(&self, bundle_path: &Path) -> io::Result<()> {
        let status = std::process::Command::new(LSREGISTER_PATH)
            .arg("-f")
            .arg(bundle_path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("lsregister exited with {status}")))
        }
    }
}

/// A registrar that does nothing. Used by every test and by nothing else.
pub struct NoopLaunchServicesRegistrar;

impl LaunchServicesRegistrar for NoopLaunchServicesRegistrar {
    fn register(&self, _bundle_path: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// Assembles `<applications_dir>/turbofig.app`: `Contents/Info.plist`, the
/// placeholder icon, and a byte copy of `own_exe` (symlinks resolved first)
/// at `Contents/MacOS/turbofig`. Ad-hoc signs the finished bundle
/// best-effort (a failure is logged, never returned), then registers it
/// with Launch Services best-effort (same: a failure is only ever a
/// warning), so Spotlight and Launchpad find it right away.
///
/// Refuses to write a real Applications folder (`/Applications` or
/// `~/Applications`) from a debug build unless `TURBOFIG_DEV_REAL_DESKTOP=1`
/// (see `refuse_real_applications_dir`).
pub fn install_app_bundle(applications_dir: &Path, own_exe: &Path) -> io::Result<PathBuf> {
    install_app_bundle_with_signer(
        applications_dir,
        own_exe,
        &RealCodeSigner,
        &RealLaunchServicesRegistrar,
    )
}

/// The testable half of `install_app_bundle`: takes an explicit `signer`
/// and `registrar` so a test exercises the whole assembly without ever
/// shelling out to the real `codesign` or `lsregister`.
pub fn install_app_bundle_with_signer(
    applications_dir: &Path,
    own_exe: &Path,
    signer: &dyn CodeSigner,
    registrar: &dyn LaunchServicesRegistrar,
) -> io::Result<PathBuf> {
    refuse_real_applications_dir(applications_dir)?;

    let bundle_dir = applications_dir.join("turbofig.app");
    let contents_dir = bundle_dir.join("Contents");
    let macos_dir = contents_dir.join("MacOS");
    let resources_dir = contents_dir.join("Resources");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(&resources_dir)?;

    // Resolve symlinks (a Homebrew Cellar binary is reached through the
    // stable <prefix>/bin/turbofig symlink) and read the real bytes early,
    // so the build id can be stamped into the plist below; the write to
    // Contents/MacOS/turbofig itself still happens last (see below).
    let resolved_exe = own_exe.canonicalize()?;
    let exe_bytes = std::fs::read(&resolved_exe)?;
    let build_id = build_id_for_bytes(&exe_bytes);

    write_atomic(
        &contents_dir.join("Info.plist"),
        info_plist_contents(&build_id).as_bytes(),
        0o644,
    )?;
    write_atomic(&resources_dir.join("AppIcon.icns"), APP_ICON_BYTES, 0o644)?;

    // This write happens last and atomically: a crash between the
    // plist/icon write above and this one leaves a bundle with no
    // executable at all, which macOS simply refuses to launch, never a
    // half-written binary.
    write_atomic(&macos_dir.join("turbofig"), &exe_bytes, 0o755)?;

    if let Err(e) = signer.sign(&bundle_dir) {
        eprintln!(
            "turbofig: warning: could not ad-hoc sign {}: {e}",
            bundle_dir.display()
        );
    }

    if let Err(e) = registrar.register(&bundle_dir) {
        eprintln!(
            "turbofig: warning: could not register {} with Launch Services (Spotlight/Launchpad \
             may not find it until the next reindex): {e}",
            bundle_dir.display()
        );
    }

    Ok(bundle_dir)
}

/// Reads the actual on-disk directory entry name for our bundle inside
/// `applications_dir`, whatever its case: `has_our_bundle`'s own
/// ownership check (`CFBundleIdentifier` matches `BUNDLE_IDENTIFIER`), but
/// returning the literal entry name rather than a bool. Needed because
/// macOS disks are usually case-insensitive but case-preserving: a lookup
/// by either `turbofig.app` or `Turbofig.app` finds the same directory, so
/// only reading the directory listing itself tells the two apart.
fn actual_bundle_dir_name(applications_dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(applications_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.eq_ignore_ascii_case("turbofig.app") {
            continue;
        }
        let plist =
            std::fs::read_to_string(applications_dir.join(name).join("Contents/Info.plist"));
        let Ok(plist) = plist else {
            continue;
        };
        if extract_plist_string(&plist, "CFBundleIdentifier").as_deref() == Some(BUNDLE_IDENTIFIER)
        {
            return Some(name.to_owned());
        }
    }
    None
}

/// Renames an existing, differently-cased bundle (`Turbofig.app`, from
/// before the lowercase brand rename) to `turbofig.app`, then re-registers
/// it with Launch Services so Spotlight/Launchpad show the new name.
///
/// macOS disks are usually case-insensitive (but case-preserving), so a
/// direct rename to a name that differs only by case can fail or silently
/// do nothing: the kernel resolves the destination to the same file it is
/// renaming from. The rename goes through a temp name in the same folder
/// first, so it always takes effect, on a case-insensitive volume or not.
///
/// Returns `Ok(true)` when a rename actually happened, `Ok(false)` when no
/// bundle was there, it already carried the lowercase name, or it belonged
/// to someone else (never touched, same ownership check as
/// `remove_turbofig_app_bundle`).
pub fn migrate_legacy_bundle_name(
    applications_dir: &Path,
    registrar: &dyn LaunchServicesRegistrar,
) -> io::Result<bool> {
    let Some(actual_name) = actual_bundle_dir_name(applications_dir) else {
        return Ok(false);
    };
    if actual_name == "turbofig.app" {
        return Ok(false);
    }

    let legacy_dir = applications_dir.join(&actual_name);
    let new_dir = applications_dir.join("turbofig.app");
    let tmp_dir = applications_dir.join(format!(".turbofig-rename-{}.tmp", std::process::id()));
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir)?;
    }
    std::fs::rename(&legacy_dir, &tmp_dir)?;
    std::fs::rename(&tmp_dir, &new_dir)?;

    if let Err(e) = registrar.register(&new_dir) {
        eprintln!(
            "turbofig: warning: could not register {} with Launch Services after the lowercase \
             rename (Spotlight/Launchpad may not find it until the next reindex): {e}",
            new_dir.display()
        );
    }

    Ok(true)
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

/// True when `<applications_dir>/turbofig.app` exists but is stale against
/// `own_exe` (the binary that would be installed): its executable is
/// missing, its `CFBundleShortVersionString` differs from this binary's own
/// version, or (the same version, but a rebuild: see `build_id_for_bytes`)
/// its stored `TurbofigBuildId` differs from `own_exe`'s own build id.
/// False when the bundle does not exist at all: step 4 decides whether to
/// create one, never the daemon's own startup check (see `main.rs`'s
/// `run_daemon`).
///
/// The build-id comparison only ever reads `own_exe` (never the bundle's
/// installed copy): cheaper than a byte-for-byte compare of both binaries,
/// since the installed copy's id is already sitting in its `Info.plist`.
pub fn app_bundle_outdated(applications_dir: &Path, own_exe: &Path) -> bool {
    let bundle_dir = applications_dir.join("turbofig.app");
    let Ok(plist_contents) = std::fs::read_to_string(bundle_dir.join("Contents/Info.plist")) else {
        return false;
    };
    if !bundle_dir.join("Contents/MacOS/turbofig").exists() {
        return true;
    }
    let version_mismatch = extract_plist_string(&plist_contents, "CFBundleShortVersionString")
        .as_deref()
        != Some(env!("CARGO_PKG_VERSION"));
    if version_mismatch {
        return true;
    }
    let Ok(resolved_own_exe) = own_exe.canonicalize() else {
        return false;
    };
    let Ok(own_bytes) = std::fs::read(&resolved_own_exe) else {
        return false;
    };
    let own_build_id = build_id_for_bytes(&own_bytes);
    extract_plist_string(&plist_contents, "TurbofigBuildId").as_deref()
        != Some(own_build_id.as_str())
}

/// The bundle directory itself: `<applications_dir>/turbofig.app`.
pub fn app_bundle_path(applications_dir: &Path) -> PathBuf {
    applications_dir.join("turbofig.app")
}

/// The bundle's own executable: `<applications_dir>/turbofig.app/Contents/MacOS/turbofig`.
/// Used by the app LaunchAgent (`cli::run_autostart_on_app`) to pin its
/// `ProgramArguments` at the bundle itself, not the Homebrew binary.
pub fn app_bundle_executable_path(applications_dir: &Path) -> PathBuf {
    app_bundle_path(applications_dir).join("Contents/MacOS/turbofig")
}

/// True when the running binary's own canonical path is inside a `.app`
/// bundle's `Contents/MacOS/`: i.e. this process was launched by opening
/// `turbofig.app`, not by a CLI invocation. Step 2 uses this to switch into
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

/// Removes `<applications_dir>/turbofig.app`, but only if its `Info.plist`
/// carries our own `BUNDLE_IDENTIFIER`: a foreign `turbofig.app` (an
/// unrelated app that happens to share the name) is left untouched. Returns
/// `Ok(true)` when a bundle was actually removed, `Ok(false)` when none was
/// there or it belonged to someone else: both are success, not an error.
pub fn remove_turbofig_app_bundle(applications_dir: &Path) -> io::Result<bool> {
    let bundle_dir = applications_dir.join("turbofig.app");
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
            PathBuf::from("/Users/dev/Applications/turbofig.app")
        );
    }

    #[test]
    fn app_bundle_executable_path_points_at_contents_macos() {
        assert_eq!(
            app_bundle_executable_path(Path::new("/Users/dev/Applications")),
            PathBuf::from("/Users/dev/Applications/turbofig.app/Contents/MacOS/turbofig")
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

        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install_app_bundle_with_signer");

        assert_eq!(bundle_dir, applications_dir.join("turbofig.app"));
        let info_plist = bundle_dir.join("Contents/Info.plist");
        assert!(info_plist.exists());
        run_plutil_lint(&info_plist);

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn install_app_bundle_info_plist_carries_the_expected_keys() {
        let applications_dir = unique_temp_dir("keys");
        let own_exe = std::env::current_exe().expect("current_exe");

        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");
        let contents =
            std::fs::read_to_string(bundle_dir.join("Contents/Info.plist")).expect("read plist");

        assert_eq!(
            extract_plist_string(&contents, "CFBundleName").as_deref(),
            Some("turbofig")
        );
        assert_eq!(
            extract_plist_string(&contents, "CFBundleDisplayName").as_deref(),
            Some("turbofig")
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

        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
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

        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
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
        let own_exe = std::env::current_exe().expect("current_exe");
        assert!(!app_bundle_outdated(&applications_dir, &own_exe));
    }

    #[test]
    fn app_bundle_outdated_is_false_right_after_install() {
        let applications_dir = unique_temp_dir("outdated-fresh");
        let own_exe = std::env::current_exe().expect("current_exe");
        install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");

        assert!(!app_bundle_outdated(&applications_dir, &own_exe));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn app_bundle_outdated_is_true_when_the_version_differs() {
        let applications_dir = unique_temp_dir("outdated-version");
        let own_exe = std::env::current_exe().expect("current_exe");
        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");

        let plist_path = bundle_dir.join("Contents/Info.plist");
        let contents = std::fs::read_to_string(&plist_path).expect("read plist");
        let rewritten = contents.replace(env!("CARGO_PKG_VERSION"), "0.0.1-older");
        std::fs::write(&plist_path, rewritten).expect("rewrite plist with an old version");

        assert!(app_bundle_outdated(&applications_dir, &own_exe));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn app_bundle_outdated_is_true_when_the_executable_is_missing() {
        let applications_dir = unique_temp_dir("outdated-exe-missing");
        let own_exe = std::env::current_exe().expect("current_exe");
        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");

        std::fs::remove_file(bundle_dir.join("Contents/MacOS/turbofig"))
            .expect("remove copied exe");

        assert!(app_bundle_outdated(&applications_dir, &own_exe));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn app_bundle_outdated_is_true_when_the_build_id_differs_but_the_version_does_not() {
        // A rebuild with no version bump: the owner's exact reported bug.
        // `app_bundle_outdated` must still notice via `TurbofigBuildId`,
        // even though `CFBundleShortVersionString` matches.
        let applications_dir = unique_temp_dir("outdated-build-id");
        let own_exe = std::env::current_exe().expect("current_exe");
        let bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");

        let plist_path = bundle_dir.join("Contents/Info.plist");
        let contents = std::fs::read_to_string(&plist_path).expect("read plist");
        let rewritten = contents.replace(
            &extract_plist_string(&contents, "TurbofigBuildId").expect("stored build id"),
            "stale-build-id-0000000000000000-0",
        );
        std::fs::write(&plist_path, rewritten).expect("rewrite plist with a stale build id");

        assert!(app_bundle_outdated(&applications_dir, &own_exe));

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn build_id_for_bytes_differs_for_different_bytes_and_matches_for_identical_bytes() {
        assert_ne!(build_id_for_bytes(b"one"), build_id_for_bytes(b"two"));
        assert_eq!(build_id_for_bytes(b"same"), build_id_for_bytes(b"same"));
    }

    #[test]
    fn remove_turbofig_app_bundle_removes_our_own_bundle() {
        let applications_dir = unique_temp_dir("uninstall-own");
        let own_exe = std::env::current_exe().expect("current_exe");
        install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");

        let removed =
            remove_turbofig_app_bundle(&applications_dir).expect("remove_turbofig_app_bundle");

        assert!(removed);
        assert!(!applications_dir.join("turbofig.app").exists());

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
        let foreign_bundle = applications_dir.join("turbofig.app");
        let foreign_contents = foreign_bundle.join("Contents");
        std::fs::create_dir_all(&foreign_contents).expect("mkdir foreign bundle");
        std::fs::write(
            foreign_contents.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.example.other</string></dict></plist>",
        )
        .expect("write foreign plist");

        let removed =
            remove_turbofig_app_bundle(&applications_dir).expect("remove_turbofig_app_bundle");

        assert!(!removed, "a foreign turbofig.app must never be removed");
        assert!(foreign_bundle.exists());

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[cfg(debug_assertions)]
    fn assert_install_refuses(real_dir: &Path, label: &str) {
        let own_exe = std::env::current_exe().expect("current_exe");

        // The developer may have a real turbofig.app installed, so compare
        // before and after instead of asserting that nothing is there. The
        // guard must refuse before it touches the filesystem at all.
        let bundle = real_dir.join("turbofig.app");
        let snapshot = |path: &Path| -> Option<(std::time::SystemTime, Vec<u8>)> {
            let modified = std::fs::metadata(path).ok()?.modified().ok()?;
            let plist = std::fs::read(path.join("Contents/Info.plist")).unwrap_or_default();
            Some((modified, plist))
        };
        let before = snapshot(&bundle);

        let result = install_app_bundle_with_signer(
            real_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        );

        assert!(
            result.is_err(),
            "a debug build must refuse to install into the real {label}"
        );
        assert_eq!(
            snapshot(&bundle),
            before,
            "the guard must refuse before writing anything under the real {label}"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn install_app_bundle_refuses_the_real_home_applications_directory() {
        let Ok(home) = std::env::var("HOME") else {
            return; // HOME unset in this environment; nothing to guard
        };
        assert_install_refuses(&PathBuf::from(home).join("Applications"), "~/Applications");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn install_app_bundle_refuses_the_real_global_applications_directory() {
        assert_install_refuses(Path::new("/Applications"), "/Applications");
    }

    #[test]
    fn applications_dir_from_env_prefers_an_existing_global_bundle_over_home() {
        let global = unique_temp_dir("resolver-global");
        let home = unique_temp_dir("resolver-home");
        let own_exe = std::env::current_exe().expect("current_exe");
        install_app_bundle_with_signer(
            &global,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install into global");
        install_app_bundle_with_signer(
            &home,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install into home");

        assert!(has_our_bundle(&global));
        assert!(has_our_bundle(&home));

        std::fs::remove_dir_all(&global).ok();
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn has_our_bundle_is_false_for_a_directory_with_no_bundle_at_all() {
        let dir = unique_temp_dir("resolver-empty");
        assert!(!has_our_bundle(&dir));
    }

    #[test]
    fn has_our_bundle_is_false_for_a_foreign_bundle() {
        let dir = unique_temp_dir("resolver-foreign");
        let foreign_contents = dir.join("turbofig.app/Contents");
        std::fs::create_dir_all(&foreign_contents).expect("mkdir foreign bundle");
        std::fs::write(
            foreign_contents.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.example.other</string></dict></plist>",
        )
        .expect("write foreign plist");

        assert!(!has_our_bundle(&dir));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migrate_legacy_bundle_name_is_a_no_op_when_nothing_is_there() {
        let applications_dir = unique_temp_dir("migrate-missing");
        let renamed = migrate_legacy_bundle_name(&applications_dir, &NoopLaunchServicesRegistrar)
            .expect("migrate_legacy_bundle_name");
        assert!(!renamed);
    }

    #[test]
    fn migrate_legacy_bundle_name_is_a_no_op_when_already_lowercase() {
        let applications_dir = unique_temp_dir("migrate-already-lowercase");
        let own_exe = std::env::current_exe().expect("current_exe");
        install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");

        let renamed = migrate_legacy_bundle_name(&applications_dir, &NoopLaunchServicesRegistrar)
            .expect("migrate_legacy_bundle_name");

        assert!(!renamed);
        assert!(applications_dir.join("turbofig.app").exists());

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn migrate_legacy_bundle_name_never_touches_a_foreign_bundle() {
        let applications_dir = unique_temp_dir("migrate-foreign");
        let foreign_bundle = applications_dir.join("turbofig.app");
        let foreign_contents = foreign_bundle.join("Contents");
        std::fs::create_dir_all(&foreign_contents).expect("mkdir foreign bundle");
        std::fs::write(
            foreign_contents.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.example.other</string></dict></plist>",
        )
        .expect("write foreign plist");

        let renamed = migrate_legacy_bundle_name(&applications_dir, &NoopLaunchServicesRegistrar)
            .expect("migrate_legacy_bundle_name");

        assert!(!renamed, "a foreign turbofig.app must never be renamed");
        assert!(foreign_bundle.exists());

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    /// The exact migration scenario: an owner upgrading from a pre-rename
    /// install, with a real `turbofig.app` still on disk under the old,
    /// capitalized name. Exercises the 2-step rename through a temp name,
    /// which must work whether or not the volume is case-insensitive.
    #[test]
    fn migrate_legacy_bundle_name_renames_an_old_capitalized_bundle() {
        let applications_dir = unique_temp_dir("migrate-rename");
        let own_exe = std::env::current_exe().expect("current_exe");
        let lowercase_bundle_dir = install_app_bundle_with_signer(
            &applications_dir,
            &own_exe,
            &NoopCodeSigner,
            &NoopLaunchServicesRegistrar,
        )
        .expect("install");
        // Simulate a pre-rename install: move the freshly installed
        // lowercase bundle back to the old, capitalized name via the same
        // 2-step temp-name dance the migration itself uses, so this setup
        // works on a case-insensitive volume too.
        let tmp = applications_dir.join("setup-tmp.app");
        std::fs::rename(&lowercase_bundle_dir, &tmp).expect("rename to tmp");
        std::fs::rename(&tmp, applications_dir.join("Turbofig.app")).expect("rename to legacy");

        let renamed = migrate_legacy_bundle_name(&applications_dir, &NoopLaunchServicesRegistrar)
            .expect("migrate_legacy_bundle_name");

        assert!(renamed);
        assert_eq!(
            actual_bundle_dir_name(&applications_dir).as_deref(),
            Some("turbofig.app")
        );
        assert!(applications_dir
            .join("turbofig.app/Contents/MacOS/turbofig")
            .exists());

        std::fs::remove_dir_all(&applications_dir).ok();
    }

    #[test]
    fn applications_dir_from_env_honors_the_override_even_when_a_bundle_exists_elsewhere() {
        let override_dir = unique_temp_dir("resolver-override");
        // SAFETY: test-only, single-threaded env mutation, restored before
        // the function returns.
        unsafe {
            std::env::set_var("TURBOFIG_APPLICATIONS_DIR", &override_dir);
        }
        let resolved = applications_dir_from_env();
        unsafe {
            std::env::remove_var("TURBOFIG_APPLICATIONS_DIR");
        }
        assert_eq!(resolved, override_dir);
    }
}
