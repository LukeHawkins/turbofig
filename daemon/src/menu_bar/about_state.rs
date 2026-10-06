//! Pure logic for the About window: the fixed IPC command set the webview
//! is allowed to send, the `/health`-to-chip mapping shown in its header,
//! and the first-use rule that decides whether to open it automatically on
//! app start. No `wry`/`tao` dependency: `about_window.rs` is the only
//! caller, and the only place any of this touches a real window or webview.

use std::path::Path;

/// The 7 commands the About window's webview may send over IPC
/// (`window.ipc.postMessage("<command>")`). Anything else is rejected by
/// `parse_ipc_command`, never dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcCommand {
    CopyManifestPath,
    OpenFigma,
    CopyAgentPrompt,
    CopyMcpCommand,
    CopyMcpJson,
    OpenDocs,
    Quit,
}

/// Parses a raw IPC message into one of the 7 known commands. Returns
/// `None` for anything else at all: an unknown command, extra whitespace, a
/// different case, or a non-command payload. The About window's IPC handler
/// silently drops a `None`, so a stray or malformed message can never
/// trigger an action.
pub fn parse_ipc_command(raw: &str) -> Option<IpcCommand> {
    match raw {
        "copy_manifest_path" => Some(IpcCommand::CopyManifestPath),
        "open_figma" => Some(IpcCommand::OpenFigma),
        "copy_agent_prompt" => Some(IpcCommand::CopyAgentPrompt),
        "copy_mcp_command" => Some(IpcCommand::CopyMcpCommand),
        "copy_mcp_json" => Some(IpcCommand::CopyMcpJson),
        "open_docs" => Some(IpcCommand::OpenDocs),
        "quit" => Some(IpcCommand::Quit),
        _ => None,
    }
}

/// The 2 live status chips shown in the About window's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipState {
    /// "Bridge running" or "Bridge not running".
    pub bridge_chip: String,
    /// "waiting" (0 files), "Figma plugin connected: `<name>`" (1 file), or
    /// "Figma plugin connected: `<n>` files" (more than 1).
    pub figma_chip: String,
}

/// Builds the chip pair from the same connected-file list `menu_bar::state`
/// builds `MenuState` from (`/health`'s `connectedFiles`, already parsed by
/// the caller): `None` is "the daemon did not answer at all", `Some(&[])` is
/// "reachable, 0 files connected".
pub fn chips_from_connected_files(
    bridge_reachable: bool,
    connected_file_names: &[String],
) -> ChipState {
    let bridge_chip = if bridge_reachable {
        "Bridge running".to_owned()
    } else {
        "Bridge not running".to_owned()
    };
    let figma_chip = match connected_file_names {
        [] => "waiting".to_owned(),
        [only] => format!("Figma plugin connected: {only}"),
        many => format!("Figma plugin connected: {} files", many.len()),
    };
    ChipState {
        bridge_chip,
        figma_chip,
    }
}

/// True when the About window should open automatically on app start:
/// `<home>/plugin-seen` does not exist yet, i.e. no Figma plugin has ever
/// connected (the same marker `first_run`/`ws.rs` use elsewhere).
pub fn should_auto_open_about_window(home: &Path) -> bool {
    !home.join("plugin-seen").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipc_command_accepts_all_7_commands() {
        assert_eq!(
            parse_ipc_command("copy_manifest_path"),
            Some(IpcCommand::CopyManifestPath)
        );
        assert_eq!(parse_ipc_command("open_figma"), Some(IpcCommand::OpenFigma));
        assert_eq!(
            parse_ipc_command("copy_agent_prompt"),
            Some(IpcCommand::CopyAgentPrompt)
        );
        assert_eq!(
            parse_ipc_command("copy_mcp_command"),
            Some(IpcCommand::CopyMcpCommand)
        );
        assert_eq!(
            parse_ipc_command("copy_mcp_json"),
            Some(IpcCommand::CopyMcpJson)
        );
        assert_eq!(parse_ipc_command("open_docs"), Some(IpcCommand::OpenDocs));
        assert_eq!(parse_ipc_command("quit"), Some(IpcCommand::Quit));
    }

    #[test]
    fn parse_ipc_command_rejects_anything_else() {
        assert_eq!(parse_ipc_command(""), None);
        assert_eq!(parse_ipc_command("Quit"), None);
        assert_eq!(parse_ipc_command("quit "), None);
        assert_eq!(parse_ipc_command(" quit"), None);
        assert_eq!(parse_ipc_command("copy_manifest_path extra"), None);
        assert_eq!(parse_ipc_command("eval(1+1)"), None);
        assert_eq!(parse_ipc_command("{\"op\":\"quit\"}"), None);
        assert_eq!(parse_ipc_command("open_url"), None);
    }

    #[test]
    fn chips_bridge_not_running_when_unreachable() {
        let chips = chips_from_connected_files(false, &[]);
        assert_eq!(chips.bridge_chip, "Bridge not running");
    }

    #[test]
    fn chips_bridge_running_when_reachable() {
        let chips = chips_from_connected_files(true, &[]);
        assert_eq!(chips.bridge_chip, "Bridge running");
    }

    #[test]
    fn chips_figma_waiting_when_no_files() {
        let chips = chips_from_connected_files(true, &[]);
        assert_eq!(chips.figma_chip, "waiting");
    }

    #[test]
    fn chips_figma_connected_with_name_for_one_file() {
        let chips = chips_from_connected_files(true, &["Design A".to_owned()]);
        assert_eq!(chips.figma_chip, "Figma plugin connected: Design A");
    }

    #[test]
    fn chips_figma_connected_with_count_for_many_files() {
        let chips =
            chips_from_connected_files(true, &["Design A".to_owned(), "Design B".to_owned()]);
        assert_eq!(chips.figma_chip, "Figma plugin connected: 2 files");
    }

    #[test]
    fn first_use_true_when_plugin_seen_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(should_auto_open_about_window(dir.path()));
    }

    #[test]
    fn first_use_false_once_plugin_seen_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("plugin-seen"), "").expect("write marker");
        assert!(!should_auto_open_about_window(dir.path()));
    }

    /// The same embedded page `about_window.rs` loads with `include_str!`,
    /// read again here (a second compile-time literal, not a shared
    /// `pub(crate)` constant) so this pure-logic test needs no `wry`/`tao`
    /// import at all. Asserts no `src="http`/`href="http` anywhere except
    /// the "Docs" link's own `href`, the sole, deliberate exception
    /// `navigation_is_allowed` and the page's own `onclick` both account for.
    const ABOUT_HTML: &str = include_str!("../../assets/about/about.html");
    const DOCS_HREF: &str = "href=\"https://github.com/LukeHawkins/turbofig\"";

    #[test]
    fn the_about_page_has_no_remote_urls_except_the_docs_link() {
        assert!(
            ABOUT_HTML.contains(DOCS_HREF),
            "expected the one allowed docs href to be present"
        );
        let without_docs_href = ABOUT_HTML.replacen(DOCS_HREF, "", 1);
        assert!(
            !without_docs_href.contains("src=\"http"),
            "no src attribute may point at a remote URL"
        );
        assert!(
            !without_docs_href.contains("href=\"http"),
            "no href attribute other than the docs link may point at a remote URL"
        );
    }
}
