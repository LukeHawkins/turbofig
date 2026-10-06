//! Pure logic for the Settings window: the fixed IPC command set the
//! `daemon/assets/settings/settings.html` webview is allowed to send. No
//! `wry`/`tao` dependency, the same split `about_state.rs` uses for the
//! About window: `settings_window.rs` is the only caller, and the only place
//! any of this touches a real window or webview.

/// The 5 commands the Settings window's webview may send over IPC
/// (`window.ipc.postMessage("<command>")`). Anything else is rejected by
/// `parse_ipc_command`, never dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcCommand {
    /// The "Start at login" switch was turned on.
    StartAtLoginOn,
    /// The "Start at login" switch was turned off.
    StartAtLoginOff,
    CopyManifestPath,
    /// "Open plugin folder": reveals the plugin manifest (`open -R`), the
    /// same action the About window's "Show in Finder" button runs.
    OpenPluginFolder,
    /// "Open log": opens `daemon.log` in Console.
    OpenLog,
    /// The page's own script has finished running and defined the real
    /// `window.turbofigSetStatic`: re-push whatever static state Rust
    /// currently holds, in case an earlier push raced the page load and was
    /// lost (see `settings_window::handle_ipc_message`).
    PageReady,
}

/// Parses a raw IPC message into one of the 6 known commands. Returns
/// `None` for anything else at all: an unknown command, extra whitespace, a
/// different case, or a non-command payload. The Settings window's IPC
/// handler silently drops a `None`, so a stray or malformed message can
/// never trigger an action.
pub fn parse_ipc_command(raw: &str) -> Option<IpcCommand> {
    match raw {
        "start_at_login_on" => Some(IpcCommand::StartAtLoginOn),
        "start_at_login_off" => Some(IpcCommand::StartAtLoginOff),
        "copy_manifest_path" => Some(IpcCommand::CopyManifestPath),
        "open_plugin_folder" => Some(IpcCommand::OpenPluginFolder),
        "open_log" => Some(IpcCommand::OpenLog),
        "page_ready" => Some(IpcCommand::PageReady),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipc_command_accepts_all_6_commands() {
        assert_eq!(
            parse_ipc_command("start_at_login_on"),
            Some(IpcCommand::StartAtLoginOn)
        );
        assert_eq!(
            parse_ipc_command("start_at_login_off"),
            Some(IpcCommand::StartAtLoginOff)
        );
        assert_eq!(
            parse_ipc_command("copy_manifest_path"),
            Some(IpcCommand::CopyManifestPath)
        );
        assert_eq!(
            parse_ipc_command("open_plugin_folder"),
            Some(IpcCommand::OpenPluginFolder)
        );
        assert_eq!(parse_ipc_command("open_log"), Some(IpcCommand::OpenLog));
        assert_eq!(parse_ipc_command("page_ready"), Some(IpcCommand::PageReady));
    }

    #[test]
    fn parse_ipc_command_rejects_anything_else() {
        assert_eq!(parse_ipc_command(""), None);
        assert_eq!(parse_ipc_command("quit"), None);
        assert_eq!(parse_ipc_command("Start_At_Login_On"), None);
        assert_eq!(parse_ipc_command("open_log "), None);
        assert_eq!(parse_ipc_command("{\"op\":\"open_log\"}"), None);
    }

    /// The embedded settings page (`settings_window.rs`'s own `include_str!`
    /// copy), scanned the same way `about_state`'s html-scan test checks
    /// `about.html`: no remote URL anywhere, since this page has no "Docs"
    /// link or other deliberate exception at all.
    const SETTINGS_HTML: &str = include_str!("../../assets/settings/settings.html");

    #[test]
    fn the_settings_page_has_no_remote_urls() {
        assert!(!SETTINGS_HTML.contains("src=\"http"));
        assert!(!SETTINGS_HTML.contains("href=\"http"));
    }
}
