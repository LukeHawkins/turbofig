//! launchd integration for `turbofig autostart`/`uninstall`: the stable
//! binary path rule, the `eu.lukehawkins.turbofig.plist` contents, and a
//! seam over `launchctl` so tests never touch the real LaunchAgents
//! directory or the real user's `gui/<uid>` session.

use std::io;
use std::path::{Path, PathBuf};

/// The launchd service label, shared by the plist filename, `Label`, and
/// every `launchctl bootout`/`bootstrap` target. Used by the headless
/// (daemon-only, `autostart on --headless`) service.
pub const SERVICE_LABEL: &str = "eu.lukehawkins.turbofig";

/// The launchd service label for the app autostart service (`autostart on`,
/// no `--headless`): the menu-bar app itself, not only the daemon.
pub const APP_SERVICE_LABEL: &str = "eu.lukehawkins.turbofig.app";

/// Returns the plist filename for the headless service.
pub fn plist_file_name() -> String {
    format!("{SERVICE_LABEL}.plist")
}

/// Returns the plist filename for the app service.
pub fn app_plist_file_name() -> String {
    format!("{APP_SERVICE_LABEL}.plist")
}

/// The real `~/Library/LaunchAgents`, unless `TURBOFIG_LAUNCH_AGENTS_DIR` is
/// set (a test seam: a test always sets this to a temp dir instead).
pub fn launch_agents_dir_from_env() -> PathBuf {
    if let Ok(dir) = std::env::var("TURBOFIG_LAUNCH_AGENTS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home).join("Library/LaunchAgents")
}

/// Applies the stable-binary-path rule: a path under a Homebrew Cellar
/// (`<prefix>/Cellar/turbofig/<version>/...`) resolves to the stable
/// `<prefix>/bin/turbofig` symlink Homebrew maintains across upgrades, so
/// the plist never names a version-specific path that disappears on the
/// next `brew upgrade`. Any other path (a source checkout, a symlink
/// resolving outside a Cellar) is returned unchanged.
///
/// `canonical_current_exe` must already be canonicalized: the Cellar check
/// is a literal substring match on `/Cellar/turbofig/`, which only a
/// resolved path can be trusted to contain or not contain.
pub fn stable_binary_path(canonical_current_exe: &Path) -> PathBuf {
    let s = canonical_current_exe.to_string_lossy();
    const MARKER: &str = "/Cellar/turbofig/";
    match s.find(MARKER) {
        Some(idx) => {
            let prefix = &s[..idx];
            PathBuf::from(prefix).join("bin").join("turbofig")
        }
        None => canonical_current_exe.to_path_buf(),
    }
}

/// Returns true when `canonical_current_exe` resolves inside a Homebrew
/// Cellar for this package (the same marker `stable_binary_path` matches).
/// `autostart on` uses this to warn when the binary it is about to pin into the
/// plist is not a Homebrew install (for example `target/release/turbofig`
/// from a source checkout): that path breaks the service the moment the
/// build directory moves or is cleaned.
pub fn is_in_homebrew_cellar(canonical_current_exe: &Path) -> bool {
    canonical_current_exe
        .to_string_lossy()
        .contains("/Cellar/turbofig/")
}

/// Escapes the four XML special characters a plist string value might
/// contain (a path is the only untrusted-ish input here, but escape
/// unconditionally rather than assume it is always clean).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Builds the full `eu.lukehawkins.turbofig.plist` contents.
///
/// `program` is the stable binary path (see `stable_binary_path`);
/// `ProgramArguments` is `[program, "serve"]`. `log_path` is used for both
/// `StandardOutPath` and `StandardErrorPath`. Sets `RunAtLoad` true, and
/// `TURBOFIG_SUPERVISED=1` plus every pair in `extra_env` in
/// `EnvironmentVariables`, so a `TURBOFIG_*` override set at `autostart on`
/// time (bridge dir, ports, timeout) also applies to the launchd daemon, not
/// only to the one-off `autostart on` process. `extra_env` entries are
/// written in the given order; both keys and values are XML-escaped.
///
/// `KeepAlive` is `{SuccessfulExit: false}`, not plain `true`: launchd then
/// restarts the daemon only on a non-zero exit (a crash, or an intentional
/// restart exiting `supervisor::SUPERVISED_RESTART_EXIT_CODE`), never on an
/// ordinary `stop`'s exit 0. Plain `true` restarts on *every* exit including
/// a clean stop, which previously made `turbofig stop` pointless under
/// autostart: `serve` would find a daemon already running moments later and
/// (if it then exited 1, the old pre-fix behaviour) get relaunched by
/// `KeepAlive` again, forever, about every 10s.
pub fn plist_contents(program: &Path, log_path: &Path, extra_env: &[(String, String)]) -> String {
    let program = xml_escape(&program.to_string_lossy());
    let log_path = xml_escape(&log_path.to_string_lossy());
    let mut extra_env_xml = String::new();
    for (key, value) in extra_env {
        extra_env_xml.push_str(&format!(
            "\t\t<key>{}</key>\n\t\t<string>{}</string>\n",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{SERVICE_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
		<string>serve</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>EnvironmentVariables</key>
	<dict>
		<key>TURBOFIG_SUPERVISED</key>
		<string>1</string>
{extra_env_xml}	</dict>
	<key>StandardOutPath</key>
	<string>{log_path}</string>
	<key>StandardErrorPath</key>
	<string>{log_path}</string>
</dict>
</plist>
"#
    )
}

/// Builds the full `eu.lukehawkins.turbofig.app.plist` contents: the app
/// autostart service (`autostart on`, the default, no `--headless`).
///
/// `bundle_exe` is `Turbofig.app`'s own executable
/// (`app_bundle::app_bundle_executable_path`); `ProgramArguments` is just
/// `[bundle_exe]`, no extra argument, so launchd invokes the bare command,
/// which (`main.rs`'s `cmd_run_or_app_mode`) dispatches into the menu-bar
/// app because the path resolves inside a `.app/Contents/MacOS/`. `KeepAlive`
/// is plain `false`, unlike the headless service's `{SuccessfulExit: false}`:
/// a user who quits the app (its own "Quit Turbofig") keeps it quit until the
/// next login, rather than launchd relaunching it right away. `extra_env`
/// carries the same `TURBOFIG_*` overrides the headless plist does, but never
/// `TURBOFIG_SUPERVISED`: the app is not `serve`, so the supervised-restart
/// loop in `main.rs` never applies to it.
pub fn app_plist_contents(
    bundle_exe: &Path,
    log_path: &Path,
    extra_env: &[(String, String)],
) -> String {
    let program = xml_escape(&bundle_exe.to_string_lossy());
    let log_path = xml_escape(&log_path.to_string_lossy());
    let mut extra_env_xml = String::new();
    for (key, value) in extra_env {
        extra_env_xml.push_str(&format!(
            "		<key>{}</key>\n		<string>{}</string>\n",
            xml_escape(key),
            xml_escape(value)
        ));
    }
    let env_dict = if extra_env_xml.is_empty() {
        String::new()
    } else {
        format!("	<key>EnvironmentVariables</key>\n	<dict>\n{extra_env_xml}	</dict>\n")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{APP_SERVICE_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
{env_dict}	<key>StandardOutPath</key>
	<string>{log_path}</string>
	<key>StandardErrorPath</key>
	<string>{log_path}</string>
</dict>
</plist>
"#
    )
}

/// `TURBOFIG_*` variables that name a filesystem path. A value copied into
/// the launchd plist verbatim must be made absolute first: launchd runs the
/// daemon with its working directory at `/`, so a relative value here would
/// resolve somewhere else than the `autostart on`-time user intended.
const PATH_VALUED_VARS: &[&str] = &["TURBOFIG_BRIDGE_DIR"];

/// Collects every `TURBOFIG_*` environment variable set in the current
/// process, except `TURBOFIG_SUPERVISED` (the daemon sets its own) and
/// `TURBOFIG_LAUNCH_AGENTS_DIR` (an `autostart`-only seam, never read by the
/// daemon). Used to carry an `autostart on`-time override (bridge dir, ports,
/// timeout) into the launchd plist so the daemon sees the same values.
///
/// Uses `std::env::vars_os` rather than `std::env::vars`: the latter panics
/// on any non-UTF-8 environment variable anywhere in the process
/// environment, not only a `TURBOFIG_*` one, so a single unrelated
/// non-UTF-8-valued variable (not uncommon on a real machine) would crash
/// `turbofig autostart on` entirely. A non-UTF-8 `TURBOFIG_*` key or value is
/// skipped with a warning instead: the daemon could not read it as a path or
/// port either.
///
/// A path-valued variable (`PATH_VALUED_VARS`) is made absolute, resolved
/// against the current working directory at `autostart on` time, before being
/// carried into the plist: launchd runs the daemon with cwd `/`, so a
/// relative `TURBOFIG_BRIDGE_DIR` set at `autostart on` time would otherwise resolve
/// to a different directory once the service is actually running.
pub fn carry_over_turbofig_env() -> Vec<(String, String)> {
    let autostart_cwd = std::env::current_dir().ok();
    carry_over_turbofig_env_from(std::env::vars_os(), autostart_cwd.as_deref())
}

/// The pure, testable half of `carry_over_turbofig_env`: filters, resolves,
/// and sorts a given iterator of environment pairs instead of reading the
/// real process environment, so a test never has to mutate global state.
fn carry_over_turbofig_env_from(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    cwd: Option<&Path>,
) -> Vec<(String, String)> {
    let mut vars: Vec<(String, String)> = vars
        .filter_map(|(key, value)| {
            let key = key.to_str()?.to_owned();
            if !key.starts_with("TURBOFIG_")
                || key == "TURBOFIG_SUPERVISED"
                || key == "TURBOFIG_LAUNCH_AGENTS_DIR"
            {
                return None;
            }
            let Some(value) = value.to_str() else {
                eprintln!(
                    "turbofig autostart: skipping {key}: its value is not valid UTF-8, so it cannot \
                     be carried into the launchd plist"
                );
                return None;
            };
            let value = if PATH_VALUED_VARS.contains(&key.as_str()) {
                absolutize(value, cwd)
            } else {
                value.to_owned()
            };
            Some((key, value))
        })
        .collect();
    vars.sort();
    vars
}

/// Makes `value` absolute by joining it onto `cwd`, when `value` is relative
/// and `cwd` is available. Returns `value` unchanged when it is already
/// absolute, or when `cwd` could not be determined.
fn absolutize(value: &str, cwd: Option<&Path>) -> String {
    let path = Path::new(value);
    if path.is_absolute() {
        return value.to_owned();
    }
    match cwd {
        Some(cwd) => cwd.join(path).to_string_lossy().into_owned(),
        None => value.to_owned(),
    }
}

/// Seam over the two `launchctl` subcommands `autostart on`/`autostart off` need.
/// `RealLaunchctl` shells out for real; tests use a fake that records calls
/// instead of touching the real user's `gui/<uid>` session.
pub trait Launchctl {
    /// Runs `launchctl bootout <service_target>` (e.g. `gui/501/eu.lukehawkins.turbofig`).
    /// A "not loaded" failure is expected and must not be treated as an error.
    fn bootout(&self, service_target: &str);

    /// Runs `launchctl bootstrap <domain_target> <plist_path>`
    /// (e.g. `gui/501` and the plist path). Returns an error if the command
    /// itself fails to run or exits non-zero.
    fn bootstrap(&self, domain_target: &str, plist_path: &Path) -> io::Result<()>;
}

/// The real `launchctl`-shelling implementation. Used only by the `turbofig`
/// binary's `autostart`/`uninstall` commands, never by a test.
pub struct RealLaunchctl;

impl Launchctl for RealLaunchctl {
    fn bootout(&self, service_target: &str) {
        // Ignore the exit status: "service is not loaded" is the common and
        // expected case (first-ever `autostart on`, or a prior crash that
        // already unloaded it), not a failure worth reporting.
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", service_target])
            .status();
    }

    fn bootstrap(&self, domain_target: &str, plist_path: &Path) -> io::Result<()> {
        let status = std::process::Command::new("launchctl")
            .args(["bootstrap", domain_target])
            .arg(plist_path)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "launchctl bootstrap {domain_target} {} failed ({status})",
                plist_path.display()
            )))
        }
    }
}

/// Returns `gui/<uid>/eu.lukehawkins.turbofig`, the headless service's
/// bootout target.
pub fn service_target(uid: &str) -> String {
    format!("gui/{uid}/{SERVICE_LABEL}")
}

/// Returns `gui/<uid>/eu.lukehawkins.turbofig.app`, the app service's
/// bootout target.
pub fn app_service_target(uid: &str) -> String {
    format!("gui/{uid}/{APP_SERVICE_LABEL}")
}

/// Returns `gui/<uid>`, the bootstrap domain target.
pub fn domain_target(uid: &str) -> String {
    format!("gui/{uid}")
}

/// Runs `id -u` to get the current user's numeric uid, trimmed.
/// Only `autostart`/`uninstall` call this; it is never needed by a test, which
/// passes its own fixed uid string to the pure functions above.
pub fn current_uid() -> io::Result<String> {
    let out = std::process::Command::new("id").arg("-u").output()?;
    if !out.status.success() {
        return Err(io::Error::other("id -u failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn stable_binary_path_rewrites_an_intel_cellar_path() {
        let path = PathBuf::from("/usr/local/Cellar/turbofig/1.2.3/bin/turbofig");
        assert_eq!(
            stable_binary_path(&path),
            PathBuf::from("/usr/local/bin/turbofig")
        );
    }

    #[test]
    fn stable_binary_path_rewrites_an_apple_silicon_cellar_path() {
        let path = PathBuf::from("/opt/homebrew/Cellar/turbofig/1.2.3/bin/turbofig");
        assert_eq!(
            stable_binary_path(&path),
            PathBuf::from("/opt/homebrew/bin/turbofig")
        );
    }

    #[test]
    fn stable_binary_path_leaves_a_non_cellar_path_unchanged() {
        let path = PathBuf::from("/Users/dev/turbofig/target/release/turbofig");
        assert_eq!(stable_binary_path(&path), path);
    }

    #[test]
    fn is_in_homebrew_cellar_is_true_for_a_cellar_path() {
        let path = PathBuf::from("/opt/homebrew/Cellar/turbofig/1.2.3/bin/turbofig");
        assert!(is_in_homebrew_cellar(&path));
    }

    #[test]
    fn is_in_homebrew_cellar_is_false_for_a_dev_checkout_path() {
        let path = PathBuf::from("/Users/dev/turbofig/target/release/turbofig");
        assert!(!is_in_homebrew_cellar(&path));
    }

    #[test]
    fn stable_binary_path_leaves_a_symlinked_but_resolved_non_cellar_path_unchanged() {
        // A canonicalized path that resolved outside any Cellar (e.g. a dev
        // symlink into a checkout) must never be rewritten: the Cellar
        // check is a literal substring match, so only a real Cellar path
        // triggers it.
        let path = PathBuf::from("/Users/dev/.local/bin/turbofig");
        assert_eq!(stable_binary_path(&path), path);
    }

    #[test]
    fn plist_contents_carries_the_stable_path_serve_and_supervised_env() {
        let xml = plist_contents(
            Path::new("/opt/homebrew/bin/turbofig"),
            Path::new("/Users/dev/.turbofig/daemon.log"),
            &[],
        );
        assert!(xml.contains("<string>eu.lukehawkins.turbofig</string>"));
        assert!(xml.contains("<string>/opt/homebrew/bin/turbofig</string>"));
        assert!(xml.contains("<string>serve</string>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(xml.contains(
            "<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>"
        ));
        assert!(xml.contains("<key>TURBOFIG_SUPERVISED</key>\n\t\t<string>1</string>"));
        assert!(xml.contains("/Users/dev/.turbofig/daemon.log"));
    }

    /// Guards the exact fix for the restart-loop bug: `KeepAlive` must never
    /// be plain `true`, which restarts the daemon on *every* exit including
    /// an ordinary `stop`.
    #[test]
    fn plist_contents_never_uses_plain_keep_alive_true() {
        let xml = plist_contents(
            Path::new("/opt/homebrew/bin/turbofig"),
            Path::new("/Users/dev/.turbofig/daemon.log"),
            &[],
        );
        assert!(!xml.contains("<key>KeepAlive</key>\n\t<true/>"));
    }

    #[test]
    fn plist_contents_escapes_xml_special_characters_in_paths() {
        let xml = plist_contents(Path::new("/tmp/a&b"), Path::new("/tmp/log"), &[]);
        assert!(xml.contains("a&amp;b"));
        assert!(!xml.contains("a&b<"));
    }

    #[test]
    fn plist_contents_carries_extra_env_vars_with_xml_escaping() {
        let xml = plist_contents(
            Path::new("/opt/homebrew/bin/turbofig"),
            Path::new("/tmp/log"),
            &[
                ("TURBOFIG_BRIDGE_DIR".to_owned(), "/tmp/a&b".to_owned()),
                ("TURBOFIG_MCP_PORT".to_owned(), "18999".to_owned()),
            ],
        );
        assert!(xml.contains("<key>TURBOFIG_BRIDGE_DIR</key>\n\t\t<string>/tmp/a&amp;b</string>"));
        assert!(xml.contains("<key>TURBOFIG_MCP_PORT</key>\n\t\t<string>18999</string>"));
        // Still carries the daemon's own supervised flag alongside the extras.
        assert!(xml.contains("<key>TURBOFIG_SUPERVISED</key>\n\t\t<string>1</string>"));
    }

    fn osstr_pairs(given: Vec<(&str, &str)>) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        given
            .into_iter()
            .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
            .collect()
    }

    #[test]
    fn carry_over_turbofig_env_from_excludes_supervised_and_launch_agents_dir() {
        let given = osstr_pairs(vec![
            ("TURBOFIG_BRIDGE_DIR", "/tmp/carry-over-test"),
            ("TURBOFIG_SUPERVISED", "1"),
            ("TURBOFIG_LAUNCH_AGENTS_DIR", "/tmp/agents"),
            ("PATH", "/usr/bin"),
        ]);

        let vars = carry_over_turbofig_env_from(given.into_iter(), None);

        assert_eq!(
            vars,
            vec![(
                "TURBOFIG_BRIDGE_DIR".to_owned(),
                "/tmp/carry-over-test".to_owned()
            )]
        );
    }

    #[test]
    fn carry_over_turbofig_env_from_sorts_by_key() {
        let given = osstr_pairs(vec![
            ("TURBOFIG_WS_PORT", "18847"),
            ("TURBOFIG_BRIDGE_DIR", "/tmp/x"),
        ]);

        let vars = carry_over_turbofig_env_from(given.into_iter(), None);

        assert_eq!(
            vars,
            vec![
                ("TURBOFIG_BRIDGE_DIR".to_owned(), "/tmp/x".to_owned()),
                ("TURBOFIG_WS_PORT".to_owned(), "18847".to_owned()),
            ]
        );
    }

    #[test]
    fn carry_over_turbofig_env_from_skips_a_non_utf8_value() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let non_utf8_value = std::ffi::OsString::from_vec(vec![0x66, 0x6f, 0xff, 0x6f]);
            let given = vec![
                (
                    std::ffi::OsString::from("TURBOFIG_BRIDGE_DIR"),
                    non_utf8_value,
                ),
                (
                    std::ffi::OsString::from("TURBOFIG_WS_PORT"),
                    std::ffi::OsString::from("18847"),
                ),
            ];

            let vars = carry_over_turbofig_env_from(given.into_iter(), None);

            assert_eq!(
                vars,
                vec![("TURBOFIG_WS_PORT".to_owned(), "18847".to_owned())],
                "a non-UTF-8 value must be skipped, not panic or corrupt other entries"
            );
        }
    }

    #[test]
    fn carry_over_turbofig_env_from_makes_a_relative_bridge_dir_absolute() {
        let given = osstr_pairs(vec![("TURBOFIG_BRIDGE_DIR", "my-bridge-dir")]);

        let vars = carry_over_turbofig_env_from(given.into_iter(), Some(Path::new("/home/alice")));

        assert_eq!(
            vars,
            vec![(
                "TURBOFIG_BRIDGE_DIR".to_owned(),
                "/home/alice/my-bridge-dir".to_owned()
            )]
        );
    }

    #[test]
    fn carry_over_turbofig_env_from_leaves_an_already_absolute_bridge_dir_unchanged() {
        let given = osstr_pairs(vec![("TURBOFIG_BRIDGE_DIR", "/already/absolute")]);

        let vars = carry_over_turbofig_env_from(given.into_iter(), Some(Path::new("/home/alice")));

        assert_eq!(
            vars,
            vec![(
                "TURBOFIG_BRIDGE_DIR".to_owned(),
                "/already/absolute".to_owned()
            )]
        );
    }

    #[test]
    fn service_target_and_domain_target_are_formatted_correctly() {
        assert_eq!(service_target("501"), "gui/501/eu.lukehawkins.turbofig");
        assert_eq!(domain_target("501"), "gui/501");
    }

    #[test]
    fn app_service_target_is_formatted_correctly() {
        assert_eq!(
            app_service_target("501"),
            "gui/501/eu.lukehawkins.turbofig.app"
        );
    }

    #[test]
    fn app_plist_file_name_is_the_app_label_plus_plist() {
        assert_eq!(app_plist_file_name(), "eu.lukehawkins.turbofig.app.plist");
    }

    #[test]
    fn app_plist_contents_carries_the_bundle_exe_run_at_load_and_plain_keep_alive_false() {
        let xml = app_plist_contents(
            Path::new("/Users/dev/Applications/Turbofig.app/Contents/MacOS/turbofig"),
            Path::new("/Users/dev/.turbofig/daemon.log"),
            &[],
        );
        assert!(xml.contains("<string>eu.lukehawkins.turbofig.app</string>"));
        assert!(xml.contains(
            "<string>/Users/dev/Applications/Turbofig.app/Contents/MacOS/turbofig</string>"
        ));
        // Exactly 1 ProgramArguments entry: no "serve" argument, unlike the
        // headless plist.
        assert!(!xml.contains("<string>serve</string>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(xml.contains("<key>KeepAlive</key>\n\t<false/>"));
        assert!(
            !xml.contains("TURBOFIG_SUPERVISED"),
            "the app plist must never set TURBOFIG_SUPERVISED"
        );
    }

    #[test]
    fn app_plist_contents_carries_extra_env_vars() {
        let xml = app_plist_contents(
            Path::new("/Applications/Turbofig.app/Contents/MacOS/turbofig"),
            Path::new("/tmp/log"),
            &[("TURBOFIG_MCP_PORT".to_owned(), "18999".to_owned())],
        );
        assert!(xml.contains("<key>TURBOFIG_MCP_PORT</key>"));
        assert!(xml.contains("<string>18999</string>"));
    }

    #[test]
    fn app_plist_contents_omits_the_environment_variables_dict_with_no_extra_env() {
        let xml = app_plist_contents(
            Path::new("/Applications/Turbofig.app/Contents/MacOS/turbofig"),
            Path::new("/tmp/log"),
            &[],
        );
        assert!(!xml.contains("<key>EnvironmentVariables</key>"));
    }

    /// Records calls instead of touching a real launchd session.
    struct FakeLaunchctl {
        calls: RefCell<Vec<String>>,
        bootstrap_fails: bool,
    }

    impl FakeLaunchctl {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                bootstrap_fails: false,
            }
        }

        fn failing() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                bootstrap_fails: true,
            }
        }
    }

    impl Launchctl for FakeLaunchctl {
        fn bootout(&self, service_target: &str) {
            self.calls
                .borrow_mut()
                .push(format!("bootout {service_target}"));
        }

        fn bootstrap(&self, domain_target: &str, plist_path: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push(format!(
                "bootstrap {domain_target} {}",
                plist_path.display()
            ));
            if self.bootstrap_fails {
                Err(io::Error::other("fake bootstrap failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn fake_launchctl_records_bootout_then_bootstrap() {
        let fake = FakeLaunchctl::new();
        fake.bootout(&service_target("501"));
        fake.bootstrap(&domain_target("501"), Path::new("/tmp/x.plist"))
            .expect("fake bootstrap succeeds by default");
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "bootout gui/501/eu.lukehawkins.turbofig".to_owned(),
                "bootstrap gui/501 /tmp/x.plist".to_owned(),
            ]
        );
    }

    #[test]
    fn fake_launchctl_can_simulate_a_bootstrap_failure() {
        let fake = FakeLaunchctl::failing();
        let err = fake
            .bootstrap(&domain_target("501"), Path::new("/tmp/x.plist"))
            .unwrap_err();
        assert!(err.to_string().contains("fake bootstrap failure"));
    }
}
