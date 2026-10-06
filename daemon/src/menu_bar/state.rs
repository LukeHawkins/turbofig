//! `MenuState`: the pure, testable translation from a `/health` JSON body
//! (or `None`, when the daemon did not answer) into everything the menu
//! needs to show. No tray-icon, no tao, no I/O: `mod.rs` is the only caller,
//! and it is the only place this ever touches a real tray icon or menu.

use crate::agent_prompt::{fill_agent_prompt, ConnectedFile, TEMPLATE};

/// Which of the 2 checked-in tray-icon PNGs to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    /// At least 1 Figma file is connected.
    Normal,
    /// No file connected, or the daemon is unreachable.
    Dimmed,
}

/// One connected Figma file, the menu-bar's own copy of `/health`'s
/// `connectedFiles` entries (only the 2 fields the menu needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedFileInfo {
    pub file_key: String,
    pub name: String,
}

/// Everything the menu needs to render: the disabled header, the disabled
/// status line, which icon to show, and the filled agent-connect prompt
/// (see `agent_prompt::fill_agent_prompt`) for "Copy Agent Prompt".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    pub header: String,
    pub status_text: String,
    pub icon_state: IconState,
    pub agent_prompt: String,
}

/// Reads `/health`'s `connectedFiles` array (see
/// `AppState::named_connections_json`) out of a parsed health body. Returns
/// an empty list for anything unexpected (field missing, wrong shape, entry
/// missing `fileKey`/`name`): a malformed or reduced (unauthenticated)
/// payload must read as "no files", never panic or silently drop only some
/// entries in a way that is hard to notice.
fn connected_files_from_health(health: Option<&serde_json::Value>) -> Vec<ConnectedFileInfo> {
    let Some(health) = health else {
        return Vec::new();
    };
    health
        .get("connectedFiles")
        .and_then(|v| v.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    let file_key = f.get("fileKey")?.as_str()?.to_owned();
                    let name = f.get("name")?.as_str()?.to_owned();
                    Some(ConnectedFileInfo { file_key, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The connected files' names only. No longer called: the About window
/// dropped its live status display when its page lost all JS (see
/// `about_window.rs`). Kept, not deleted, since `state.rs` is outside the
/// scope of that change; `#[allow(dead_code)]` silences the resulting
/// warning rather than removing a function this change did not otherwise
/// touch.
#[allow(dead_code)]
pub(crate) fn connected_file_names_from_health(health: Option<&serde_json::Value>) -> Vec<String> {
    connected_files_from_health(health)
        .into_iter()
        .map(|f| f.name)
        .collect()
}

/// Builds the full `MenuState` from `/health`'s parsed body (`None` when the
/// daemon did not answer), the bridge directory and the MCP port (both
/// already known locally; never read from `/health`).
///
/// Status line and icon:
/// - `None` (unreachable): "Bridge not running", dimmed.
/// - `Some`, 0 connected files: "Waiting for the Figma plugin", dimmed.
/// - `Some`, 1 file: "Connected: `<name>`", normal.
/// - `Some`, more than 1: "Connected: `<n>` files", normal.
pub fn build_menu_state(
    health: Option<&serde_json::Value>,
    bridge_dir: &str,
    mcp_port: u16,
) -> MenuState {
    let header = format!("turbofig {}", env!("CARGO_PKG_VERSION"));
    let connected = connected_files_from_health(health);

    let (status_text, icon_state) = if health.is_none() {
        ("Bridge not running".to_owned(), IconState::Dimmed)
    } else {
        match connected.as_slice() {
            [] => ("Waiting for the Figma plugin".to_owned(), IconState::Dimmed),
            [only] => (format!("Connected: {}", only.name), IconState::Normal),
            many => (
                format!("Connected: {} files", many.len()),
                IconState::Normal,
            ),
        }
    };

    let prompt_files: Vec<ConnectedFile> = connected
        .iter()
        .map(|f| ConnectedFile {
            file_key: f.file_key.clone(),
            name: f.name.clone(),
        })
        .collect();
    let agent_prompt = fill_agent_prompt(TEMPLATE, &prompt_files, bridge_dir, mcp_port);

    MenuState {
        header,
        status_text,
        icon_state,
        agent_prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health_with_files(files: &[(&str, &str)]) -> serde_json::Value {
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptimeSeconds": 1,
            "connectedFiles": files.iter().map(|(key, name)| serde_json::json!({
                "fileKey": key,
                "name": name,
                "pluginVersion": env!("CARGO_PKG_VERSION"),
            })).collect::<Vec<_>>(),
            "pid": 1,
            "supervised": false,
        })
    }

    #[test]
    fn unreachable_daemon_shows_bridge_not_running_dimmed() {
        let state = build_menu_state(None, "/tmp/bridge", 18846);
        assert_eq!(state.status_text, "Bridge not running");
        assert_eq!(state.icon_state, IconState::Dimmed);
    }

    #[test]
    fn zero_files_shows_waiting_for_the_plugin_dimmed() {
        let health = health_with_files(&[]);
        let state = build_menu_state(Some(&health), "/tmp/bridge", 18846);
        assert_eq!(state.status_text, "Waiting for the Figma plugin");
        assert_eq!(state.icon_state, IconState::Dimmed);
    }

    #[test]
    fn one_file_shows_connected_with_its_name_normal() {
        let health = health_with_files(&[("key1", "Design A")]);
        let state = build_menu_state(Some(&health), "/tmp/bridge", 18846);
        assert_eq!(state.status_text, "Connected: Design A");
        assert_eq!(state.icon_state, IconState::Normal);
    }

    #[test]
    fn many_files_shows_the_plural_count_normal() {
        let health = health_with_files(&[("key1", "Design A"), ("key2", "Design B")]);
        let state = build_menu_state(Some(&health), "/tmp/bridge", 18846);
        assert_eq!(state.status_text, "Connected: 2 files");
        assert_eq!(state.icon_state, IconState::Normal);
    }

    #[test]
    fn header_carries_the_crate_version() {
        let state = build_menu_state(None, "/tmp/bridge", 18846);
        assert_eq!(
            state.header,
            format!("turbofig {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn agent_prompt_reflects_the_connected_files() {
        let zero = build_menu_state(Some(&health_with_files(&[])), "/tmp/bridge", 18846);
        assert!(zero.agent_prompt.contains("Run the status op first"));

        let one = build_menu_state(
            Some(&health_with_files(&[("key1", "Design A")])),
            "/tmp/bridge",
            18846,
        );
        assert!(one.agent_prompt.contains("\"fileKey\":\"key1\""));

        let many = build_menu_state(
            Some(&health_with_files(&[
                ("key1", "Design A"),
                ("key2", "Design B"),
            ])),
            "/tmp/bridge",
            18846,
        );
        assert!(many
            .agent_prompt
            .contains("Connected files: Design A (key1), Design B (key2)."));
    }

    #[test]
    fn a_malformed_connected_files_entry_is_dropped_not_panicked() {
        let health = serde_json::json!({
            "connectedFiles": [{"fileKey": "key1"}, {"fileKey": "key2", "name": "Design B"}],
        });
        let state = build_menu_state(Some(&health), "/tmp/bridge", 18846);
        assert_eq!(state.status_text, "Connected: Design B");
    }
}
