//! Text and seams for the bare `turbofig` command (no subcommand): the
//! first-run walkthrough (add the plugin, connect an agent) and the short
//! status a later run prints once `<home>/plugin-seen` already exists.
//!
//! The clipboard copy and the Figma launch are both best-effort side
//! effects with no bearing on exit status, so both are behind a seam
//! (`Clipboard`, `AppOpener`): a test fakes them instead of shelling out to
//! the real `pbcopy` or `open -a Figma`.

use std::path::Path;

/// Copies text to the system clipboard. `RealClipboard` shells out to
/// `pbcopy`; a test uses `FakeClipboard` instead.
pub trait Clipboard {
    /// Attempts the copy. Returns whether it succeeded; a caller never fails
    /// outright on a clipboard miss, since the manifest path is also printed.
    fn copy(&self, text: &str) -> bool;
}

/// The real clipboard: pipes `text` into `pbcopy`.
pub struct RealClipboard;

impl Clipboard for RealClipboard {
    fn copy(&self, text: &str) -> bool {
        use std::io::Write;
        let mut child = match std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        let Some(stdin) = child.stdin.as_mut() else {
            return false;
        };
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
        child.wait().map(|status| status.success()).unwrap_or(false)
    }
}

/// A fake clipboard for tests: records the copied text to a file instead of
/// touching the real clipboard. Always reports success.
pub struct FakeClipboard {
    record_path: std::path::PathBuf,
}

impl FakeClipboard {
    pub fn new(record_path: std::path::PathBuf) -> Self {
        Self { record_path }
    }
}

impl Clipboard for FakeClipboard {
    fn copy(&self, text: &str) -> bool {
        std::fs::write(&self.record_path, text).is_ok()
    }
}

/// A clipboard that touches nothing and always reports failure. Used in a
/// debug build when `TURBOFIG_DEV_REAL_DESKTOP` is not set and no
/// `TURBOFIG_TEST_FAKE_CLIPBOARD` fake is requested: the copy must go
/// nowhere, so the caller must also be told it did not happen, and print the
/// manifest path to copy by hand instead of claiming "it is on your
/// clipboard".
pub struct NullClipboard;

impl Clipboard for NullClipboard {
    fn copy(&self, _text: &str) -> bool {
        false
    }
}

/// Opens Figma Desktop, any other macOS app by name, or a URL in the
/// default browser. `RealAppOpener` shells out to `open`; a test uses
/// `FakeOpener` instead. Shared by the bare `turbofig` command
/// (`open_figma`), the menu bar's "Open Log" item (`open_app_with_path`,
/// e.g. `open -a Console <path>`), and the About window's "Docs" link
/// (`open_url`), so all 3 go through the same seam and the same debug-build
/// guard.
pub trait AppOpener {
    /// Attempts to open Figma Desktop. Returns whether it succeeded.
    fn open_figma(&self) -> bool;
    /// Opens `path` with the named macOS app (`open -a <app> <path>`).
    /// Returns whether it succeeded.
    fn open_app_with_path(&self, app: &str, path: &Path) -> bool;
    /// Opens `url` in the default browser (`open <url>`). Returns whether
    /// it succeeded.
    fn open_url(&self, url: &str) -> bool;
    /// Opens `path` (a `.app` bundle) as a new instance (`open -n <path>`),
    /// even if one is already running. Used only by the app's own
    /// self-relaunch (`self_update`): the running instance is already
    /// mid-exit by the time this runs, so `-n` guarantees a fresh launch
    /// rather than macOS just activating the (about to disappear) old one.
    fn open_new_instance(&self, path: &Path) -> bool;
}

/// The real opener: `open -a Figma`, macOS-only (the whole daemon is).
pub struct RealAppOpener;

impl AppOpener for RealAppOpener {
    fn open_figma(&self) -> bool {
        std::process::Command::new("open")
            .args(["-a", "Figma"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn open_app_with_path(&self, app: &str, path: &Path) -> bool {
        std::process::Command::new("open")
            .args(["-a", app])
            .arg(path)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn open_url(&self, url: &str) -> bool {
        std::process::Command::new("open")
            .arg(url)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn open_new_instance(&self, path: &Path) -> bool {
        std::process::Command::new("open")
            .arg("-n")
            .arg(path)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

/// A fake opener for tests: reports a fixed result, never spawns a process.
pub struct FakeOpener {
    succeeds: bool,
}

impl FakeOpener {
    pub fn new(succeeds: bool) -> Self {
        Self { succeeds }
    }
}

impl AppOpener for FakeOpener {
    fn open_figma(&self) -> bool {
        self.succeeds
    }

    fn open_app_with_path(&self, _app: &str, _path: &Path) -> bool {
        self.succeeds
    }

    fn open_url(&self, _url: &str) -> bool {
        self.succeeds
    }

    fn open_new_instance(&self, _path: &Path) -> bool {
        self.succeeds
    }
}

/// The first-run walkthrough `turbofig` prints once it has confirmed the
/// daemon is healthy and `<home>/plugin-seen` does not exist yet.
///
/// `binary_path` must already be the stable path (the Homebrew
/// `<prefix>/bin/turbofig` symlink when running from a Cellar, otherwise the
/// running binary's own path; see `launchd::stable_binary_path`): this
/// function only ever formats whatever path it is given, so the Cellar rule
/// itself is exercised and tested once, in `launchd.rs`.
///
/// `figma_opened` is whether `AppOpener::open_figma` already succeeded: when
/// true, the first line of step 1 reads "Figma is opening now."; when false
/// (no Figma Desktop installed, or `open` failed for any other reason), it
/// reads "Open Figma Desktop." instead.
///
/// `clipboard_copied` is whether `Clipboard::copy` already succeeded: when
/// true, step 1's paste line reads "it is on your clipboard"; when false (no
/// `pbcopy`, or it failed for any other reason), it reads "copy this path"
/// instead. The manifest path is printed either way, so pasting it by hand
/// always works even when the clipboard copy failed.
///
/// Every indented line below uses an explicit `\n   ` inside the string
/// literal, not a `\`-continued source line followed by indentation on the
/// next line: a `\` line continuation consumes the newline *and* all leading
/// whitespace on the following source line, so writing the 3-space indent on
/// its own line there silently strips it from the output.
pub fn first_run_text(
    version: &str,
    mcp_port: u16,
    ws_port: u16,
    manifest_path: &Path,
    binary_path: &Path,
    figma_opened: bool,
    clipboard_copied: bool,
) -> String {
    let figma_line = if figma_opened {
        "Figma is opening now."
    } else {
        "Open Figma Desktop."
    };
    let clipboard_hint = if clipboard_copied {
        "it is on your clipboard"
    } else {
        "copy this path"
    };
    format!(
        "turbofig {version} is running (MCP 127.0.0.1:{mcp_port}, plugin 127.0.0.1:{ws_port}).\n\n1. Add the Figma plugin (once). {figma_line}\n   Plugins > Development > Import plugin from manifest...\n   Press Cmd+Shift+G, paste the path ({clipboard_hint}), then press Return:\n   {manifest}\n\n2. Connect your agent (once):\n   Claude Code:   claude mcp add turbofig -- turbofig mcp\n   Other MCP clients, add this server:\n     {{\"command\": \"{binary}\", \"args\": [\"mcp\"]}}\n\n3. MCP blocked on your machine? Run the plugin in Figma and click \"Copy prompt\".\n",
        manifest = manifest_path.display(),
        binary = binary_path.display(),
    )
}

/// The short 3-line status `turbofig` prints when `<home>/plugin-seen`
/// already exists: a later run, not a first one.
///
/// `connected_file_names` is empty when no plugin is currently connected; the
/// line then names the action that would fix that, rather than printing an
/// empty list.
pub fn status_text(
    version: &str,
    mcp_port: u16,
    ws_port: u16,
    connected_file_names: &[String],
    manifest_path: &Path,
) -> String {
    let connected_files = if connected_file_names.is_empty() {
        "none, open the turbofig plugin in Figma".to_owned()
    } else {
        connected_file_names.join(", ")
    };
    format!(
        "turbofig {version} is running (MCP 127.0.0.1:{mcp_port}, plugin 127.0.0.1:{ws_port}).\n\
Connected files: {connected_files}\n\
Plugin manifest: {manifest}\n",
        manifest = manifest_path.display(),
    )
}

/// The 2-line text the bare `turbofig` command prints on macOS once
/// `Turbofig.app` is installed/refreshed, the daemon is running, and the
/// bundle has been opened: the app itself (its tray icon, and the About
/// window behind "About Turbofig…") is now the onboarding surface, so the
/// terminal only needs to point at it.
pub fn app_first_run_text() -> &'static str {
    "Turbofig is now in your menu bar (look for the tf icon).\n\
     Click it and choose About Turbofig… to get started. No icon? Run: turbofig status\n"
}

/// Combines the macOS app-bundle path's 2 fallible steps (installing the
/// bundle, then opening it) into either the short app-first-run text
/// (`Ok`) or a 1-line failure reason (`Err`) for the caller to print before
/// falling back to the ordinary text walkthrough (`first_run_text`/
/// `status_text`).
///
/// `bundle_install_err` is `Some(reason)` when `install_app_bundle` itself
/// failed (the caller never attempts to open a bundle that was not
/// installed); `opened` is whether the `AppOpener` seam's `open_url` (on
/// the bundle path) succeeded, only ever checked when the install
/// succeeded.
pub fn app_first_run_outcome(
    bundle_install_err: Option<String>,
    opened: bool,
) -> Result<&'static str, String> {
    if let Some(reason) = bundle_install_err {
        return Err(format!("could not install Turbofig.app: {reason}"));
    }
    if !opened {
        return Err("could not open Turbofig.app".to_owned());
    }
    Ok(app_first_run_text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_text_names_the_figma_opening_line_when_the_opener_succeeded() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/Users/dev/.turbofig/figma-plugin/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        assert!(text.contains("Figma is opening now."));
        assert!(!text.contains("Open Figma Desktop."));
    }

    #[test]
    fn first_run_text_falls_back_to_open_figma_desktop_when_the_opener_failed() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/Users/dev/.turbofig/figma-plugin/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            false,
            true,
        );
        assert!(text.contains("Open Figma Desktop."));
        assert!(!text.contains("Figma is opening now."));
    }

    #[test]
    fn first_run_text_carries_the_version_and_both_ports() {
        let text = first_run_text(
            "1.2.3",
            19999,
            19998,
            Path::new("/tmp/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        assert!(text.contains("turbofig 1.2.3 is running"));
        assert!(text.contains("MCP 127.0.0.1:19999"));
        assert!(text.contains("plugin 127.0.0.1:19998"));
    }

    #[test]
    fn first_run_text_has_exactly_the_3_numbered_steps() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/tmp/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        assert!(text.contains("1. Add the Figma plugin"));
        assert!(text.contains("2. Connect your agent"));
        assert!(text.contains("3. MCP blocked on your machine?"));
    }

    #[test]
    fn first_run_text_carries_the_manifest_path() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/Users/dev/.turbofig/figma-plugin/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        assert!(text.contains("/Users/dev/.turbofig/figma-plugin/manifest.json"));
    }

    #[test]
    fn first_run_text_carries_the_claude_mcp_add_command() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/tmp/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        assert!(text.contains("claude mcp add turbofig -- turbofig mcp"));
    }

    /// The caller is responsible for resolving `binary_path` to the stable
    /// Homebrew symlink before calling this function (see `launchd::
    /// stable_binary_path`, exercised directly in `launchd.rs`); this test
    /// only confirms the text embeds whatever stable path it is given,
    /// whether that is a Cellar-rewritten path or a dev checkout path left
    /// unchanged.
    #[test]
    fn first_run_text_embeds_the_given_stable_binary_path_verbatim() {
        let cellar_resolved = Path::new("/opt/homebrew/bin/turbofig");
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/tmp/manifest.json"),
            cellar_resolved,
            true,
            true,
        );
        assert!(text.contains("\"command\": \"/opt/homebrew/bin/turbofig\""));
        assert!(!text.contains("/Cellar/"));

        let dev_checkout = Path::new("/Users/dev/turbofig/target/release/turbofig");
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/tmp/manifest.json"),
            dev_checkout,
            true,
            true,
        );
        assert!(text.contains("\"command\": \"/Users/dev/turbofig/target/release/turbofig\""));
    }

    #[test]
    fn first_run_text_says_it_is_on_your_clipboard_when_the_copy_succeeded() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/tmp/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        assert!(text.contains("it is on your clipboard"));
        assert!(!text.contains("copy this path"));
    }

    #[test]
    fn first_run_text_says_copy_this_path_when_the_copy_failed() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/tmp/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            false,
        );
        assert!(text.contains("copy this path"));
        assert!(!text.contains("it is on your clipboard"));
    }

    /// A `\`-continued source line that is followed by indentation on the
    /// next line silently strips that indentation from the output (the
    /// continuation eats the newline *and* every leading whitespace
    /// character). This asserts the exact indentation of every step line, so
    /// a future edit that reintroduces that pattern fails loudly instead of
    /// shipping flush-left text under a numbered step.
    #[test]
    fn first_run_text_keeps_the_3_space_indent_on_every_step_line() {
        let text = first_run_text(
            "1.2.3",
            18846,
            18847,
            Path::new("/Users/dev/.turbofig/figma-plugin/manifest.json"),
            Path::new("/opt/homebrew/bin/turbofig"),
            true,
            true,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[3],
            "   Plugins > Development > Import plugin from manifest..."
        );
        assert_eq!(
            lines[4],
            "   Press Cmd+Shift+G, paste the path (it is on your clipboard), then press Return:"
        );
        assert_eq!(
            lines[5],
            "   /Users/dev/.turbofig/figma-plugin/manifest.json"
        );
        assert_eq!(
            lines[8],
            "   Claude Code:   claude mcp add turbofig -- turbofig mcp"
        );
        assert_eq!(lines[9], "   Other MCP clients, add this server:");
        assert_eq!(
            lines[10],
            "     {\"command\": \"/opt/homebrew/bin/turbofig\", \"args\": [\"mcp\"]}"
        );
    }

    #[test]
    fn status_text_lists_none_when_no_file_is_connected() {
        let text = status_text("1.2.3", 18846, 18847, &[], Path::new("/tmp/manifest.json"));
        assert!(text.contains("Connected files: none, open the turbofig plugin in Figma"));
    }

    #[test]
    fn status_text_lists_connected_file_names() {
        let text = status_text(
            "1.2.3",
            18846,
            18847,
            &["Design A".to_owned(), "Design B".to_owned()],
            Path::new("/tmp/manifest.json"),
        );
        assert!(text.contains("Connected files: Design A, Design B"));
    }

    #[test]
    fn status_text_is_exactly_3_lines_and_names_the_manifest() {
        let text = status_text(
            "1.2.3",
            18846,
            18847,
            &[],
            Path::new("/Users/dev/.turbofig/figma-plugin/manifest.json"),
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "status text must be exactly 3 lines: {text:?}"
        );
        assert!(lines[0].starts_with("turbofig 1.2.3 is running"));
        assert!(lines[1].starts_with("Connected files:"));
        assert_eq!(
            lines[2],
            "Plugin manifest: /Users/dev/.turbofig/figma-plugin/manifest.json"
        );
    }

    #[test]
    fn fake_clipboard_records_the_copied_text_and_reports_success() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let record_path = tmp.path().join("clipboard.txt");
        let clipboard = FakeClipboard::new(record_path.clone());
        assert!(clipboard.copy("/tmp/manifest.json"));
        assert_eq!(
            std::fs::read_to_string(&record_path).expect("read record"),
            "/tmp/manifest.json"
        );
    }

    #[test]
    fn null_clipboard_always_reports_failure() {
        assert!(!NullClipboard.copy("/tmp/manifest.json"));
    }

    #[test]
    fn fake_opener_reports_the_fixed_result() {
        assert!(FakeOpener::new(true).open_figma());
        assert!(!FakeOpener::new(false).open_figma());
    }

    #[test]
    fn fake_opener_reports_the_fixed_result_for_open_app_with_path() {
        assert!(FakeOpener::new(true).open_app_with_path("Console", Path::new("/tmp/daemon.log")));
        assert!(!FakeOpener::new(false).open_app_with_path("Console", Path::new("/tmp/daemon.log")));
    }

    #[test]
    fn fake_opener_reports_the_fixed_result_for_open_url() {
        assert!(FakeOpener::new(true).open_url("https://github.com/LukeHawkins/turbofig"));
        assert!(!FakeOpener::new(false).open_url("https://github.com/LukeHawkins/turbofig"));
    }

    #[test]
    fn fake_opener_reports_the_fixed_result_for_open_new_instance() {
        assert!(FakeOpener::new(true).open_new_instance(Path::new("/Applications/Turbofig.app")));
        assert!(!FakeOpener::new(false).open_new_instance(Path::new("/Applications/Turbofig.app")));
    }

    #[test]
    fn app_first_run_text_is_exactly_2_lines_naming_the_tray_icon_and_fallback_status() {
        let text = app_first_run_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "must be exactly 2 lines: {text:?}");
        assert!(lines[0].contains("menu bar"));
        assert!(lines[0].contains("tf icon"));
        assert!(lines[1].contains("About Turbofig"));
        assert!(lines[1].contains("turbofig status"));
    }

    #[test]
    fn app_first_run_outcome_succeeds_when_installed_and_opened() {
        let result = app_first_run_outcome(None, true);
        assert_eq!(result, Ok(app_first_run_text()));
    }

    #[test]
    fn app_first_run_outcome_fails_with_the_install_reason_when_install_fails() {
        let result = app_first_run_outcome(Some("disk full".to_owned()), true);
        assert_eq!(
            result,
            Err("could not install Turbofig.app: disk full".to_owned())
        );
    }

    #[test]
    fn app_first_run_outcome_fails_when_open_fails_even_though_install_succeeded() {
        let result = app_first_run_outcome(None, false);
        assert_eq!(result, Err("could not open Turbofig.app".to_owned()));
    }

    #[test]
    fn app_first_run_outcome_never_checks_open_when_install_already_failed() {
        // opened=true here would be misleading if install failed; the
        // install error must still win.
        let result = app_first_run_outcome(Some("no space left".to_owned()), true);
        assert_eq!(
            result,
            Err("could not install Turbofig.app: no space left".to_owned())
        );
    }
}
