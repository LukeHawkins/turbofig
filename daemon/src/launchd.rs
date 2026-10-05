//! launchd integration for `turbofig setup`/`uninstall`: the stable binary
//! path rule, the `eu.lukehawkins.turbofig.plist` contents, and a seam over
//! `launchctl` so tests never touch the real LaunchAgents directory or the
//! real user's `gui/<uid>` session.

use std::io;
use std::path::{Path, PathBuf};

/// The launchd service label, shared by the plist filename, `Label`, and
/// every `launchctl bootout`/`bootstrap` target.
pub const SERVICE_LABEL: &str = "eu.lukehawkins.turbofig";

/// Returns the plist filename for the service.
pub fn plist_file_name() -> String {
    format!("{SERVICE_LABEL}.plist")
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
/// `StandardOutPath` and `StandardErrorPath`. Sets `RunAtLoad` and
/// `KeepAlive` true, and `TURBOFIG_SUPERVISED=1` in `EnvironmentVariables`
/// so the running daemon knows to watch for a Homebrew upgrade (see
/// `ARCHITECTURE.md`'s supervised-restart section).
pub fn plist_contents(program: &Path, log_path: &Path) -> String {
    let program = xml_escape(&program.to_string_lossy());
    let log_path = xml_escape(&log_path.to_string_lossy());
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
	<true/>
	<key>EnvironmentVariables</key>
	<dict>
		<key>TURBOFIG_SUPERVISED</key>
		<string>1</string>
	</dict>
	<key>StandardOutPath</key>
	<string>{log_path}</string>
	<key>StandardErrorPath</key>
	<string>{log_path}</string>
</dict>
</plist>
"#
    )
}

/// Seam over the two `launchctl` subcommands `setup`/`uninstall` need.
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
/// binary's `setup`/`uninstall` commands, never by a test.
pub struct RealLaunchctl;

impl Launchctl for RealLaunchctl {
    fn bootout(&self, service_target: &str) {
        // Ignore the exit status: "service is not loaded" is the common and
        // expected case (first-ever setup, or a prior crash that already
        // unloaded it), not a failure worth reporting.
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

/// Returns `gui/<uid>/eu.lukehawkins.turbofig`, the bootout target.
pub fn service_target(uid: &str) -> String {
    format!("gui/{uid}/{SERVICE_LABEL}")
}

/// Returns `gui/<uid>`, the bootstrap domain target.
pub fn domain_target(uid: &str) -> String {
    format!("gui/{uid}")
}

/// Runs `id -u` to get the current user's numeric uid, trimmed.
/// Only `setup`/`uninstall` call this; it is never needed by a test, which
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
        );
        assert!(xml.contains("<string>eu.lukehawkins.turbofig</string>"));
        assert!(xml.contains("<string>/opt/homebrew/bin/turbofig</string>"));
        assert!(xml.contains("<string>serve</string>"));
        assert!(xml.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(xml.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(xml.contains("<key>TURBOFIG_SUPERVISED</key>\n\t\t<string>1</string>"));
        assert!(xml.contains("/Users/dev/.turbofig/daemon.log"));
    }

    #[test]
    fn plist_contents_escapes_xml_special_characters_in_paths() {
        let xml = plist_contents(Path::new("/tmp/a&b"), Path::new("/tmp/log"));
        assert!(xml.contains("a&amp;b"));
        assert!(!xml.contains("a&b<"));
    }

    #[test]
    fn service_target_and_domain_target_are_formatted_correctly() {
        assert_eq!(service_target("501"), "gui/501/eu.lukehawkins.turbofig");
        assert_eq!(domain_target("501"), "gui/501");
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
