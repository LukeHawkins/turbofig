//! The agent-connect prompt, shared byte-for-byte with the Figma plugin's
//! own "Copy prompt" button via `prompts/agent-prompt.txt`: the daemon
//! embeds it with `include_str!`, `plugin/build-ui.ts` inlines the same file
//! into the plugin bundle. Both sides run the identical 3-way branch on the
//! connected-file count below, so the 2 copies of the prompt can never drift
//! apart. A golden test here and a matching one in
//! `plugin/src/ui/ui-logic.test.ts` fill the same template with the same
//! inputs and assert the same fixed output text.
//!
//! Not `cfg(target_os = "macos")`: `menu_bar` (macOS-only) is the only
//! caller today, but the fill logic itself is plain string work, and keeping
//! it platform-neutral lets the golden test run on every CI OS, Linux
//! included.

/// One connected Figma file, as reported by `/health`'s `connectedFiles`
/// (see `AppState::named_connections_json`).
#[derive(Debug, Clone)]
pub struct ConnectedFile {
    pub file_key: String,
    pub name: String,
}

/// The raw template text, read once at compile time from the single shared
/// source file 2 directories up (`daemon/src/../../prompts/agent-prompt.txt`).
pub const TEMPLATE: &str = include_str!("../../prompts/agent-prompt.txt");

const DEFAULT_BRIDGE_DIR: &str = "~/.turbofig";

/// Fills `template` for the given connected-file list, bridge directory and
/// MCP port.
///
/// - 0 files: the example job's `fileKey` is a `<fileKey>` placeholder, and a
///   trailing hint tells the agent to run the `status` op first to learn one.
/// - Exactly 1 file: its `fileKey` is filled directly into the example job,
///   no extra hint needed.
/// - More than 1: a `Connected files: ` line lists every name and fileKey,
///   the example job keeps the `<fileKey>` placeholder, and the hint points
///   the agent at the list.
pub fn fill_agent_prompt(
    template: &str,
    connected_files: &[ConnectedFile],
    bridge_dir: &str,
    mcp_port: u16,
) -> String {
    let (files_line, file_key, file_key_hint) = match connected_files {
        [] => (
            String::new(),
            "<fileKey>".to_owned(),
            " Run the status op first to learn the fileKey.".to_owned(),
        ),
        [only] => (String::new(), only.file_key.clone(), String::new()),
        many => {
            let list = many
                .iter()
                .map(|f| format!("{} ({})", f.name, f.file_key))
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!("Connected files: {list}.\n"),
                "<fileKey>".to_owned(),
                " Pick a fileKey from the list above.".to_owned(),
            )
        }
    };
    let home = if bridge_dir.is_empty() {
        DEFAULT_BRIDGE_DIR
    } else {
        bridge_dir
    };
    template
        .replace("{{FILES_LINE}}", &files_line)
        .replace("{{FILE_KEY}}", &file_key)
        .replace("{{FILE_KEY_HINT}}", &file_key_hint)
        .replace("{{BRIDGE_DIR}}", home)
        .replace("{{MCP_PORT}}", &mcp_port.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(file_key: &str, name: &str) -> ConnectedFile {
        ConnectedFile {
            file_key: file_key.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn no_files_tells_the_agent_to_run_status_first() {
        let result = fill_agent_prompt(TEMPLATE, &[], "/tmp/custom-bridge", 18846);
        assert!(result.contains("Run the status op first to learn the fileKey."));
        assert!(result.contains("\"fileKey\":\"<fileKey>\""));
        assert!(!result.contains("Connected files:"));
    }

    #[test]
    fn exactly_one_file_fills_its_file_key_with_no_extra_hint() {
        let result = fill_agent_prompt(
            TEMPLATE,
            &[file("ABC123fileKey", "Design A")],
            "/tmp/custom-bridge",
            18846,
        );
        assert!(result.contains("\"fileKey\":\"ABC123fileKey\""));
        assert!(!result.contains("Connected files:"));
        assert!(!result.contains("Run the status op first"));
    }

    #[test]
    fn more_than_one_file_lists_every_name_and_file_key() {
        let result = fill_agent_prompt(
            TEMPLATE,
            &[file("key1", "Design A"), file("key2", "Design B")],
            "/tmp/custom-bridge",
            18846,
        );
        assert!(result.contains("Connected files: Design A (key1), Design B (key2)."));
        assert!(result.contains("\"fileKey\":\"<fileKey>\""));
        assert!(result.contains("Pick a fileKey from the list above."));
    }

    #[test]
    fn uses_a_custom_bridge_home_when_given() {
        let result = fill_agent_prompt(TEMPLATE, &[], "/tmp/custom-bridge", 18846);
        assert!(result.contains("/tmp/custom-bridge/inbox/"));
        assert!(result.contains("/tmp/custom-bridge/outbox/"));
        assert!(!result.contains("~/.turbofig"));
    }

    #[test]
    fn falls_back_to_the_default_bridge_home_when_empty() {
        let result = fill_agent_prompt(TEMPLATE, &[], "", 18846);
        assert!(result.contains("~/.turbofig/inbox/"));
        assert!(result.contains("~/.turbofig/outbox/"));
    }

    #[test]
    fn mentions_mcp_only_as_a_fallback_with_the_correct_port() {
        let result = fill_agent_prompt(TEMPLATE, &[], "/tmp/custom-bridge", 19999);
        assert!(result.contains("http://127.0.0.1:19999/mcp"));
        assert!(!result.contains("18846"));
    }

    #[test]
    fn never_instructs_a_bare_get_or_a_web_fetch() {
        let result = fill_agent_prompt(TEMPLATE, &[], "/tmp/custom-bridge", 18846);
        assert!(!result.contains("GET "));
        assert!(result.contains("curl"));
        assert!(result.contains("web-fetch"));
    }

    #[test]
    fn tells_the_agent_to_run_turbofig_start_and_retry() {
        let result = fill_agent_prompt(TEMPLATE, &[], "/tmp/custom-bridge", 18846);
        assert!(result.contains("turbofig start"));
        assert!(result.contains("retry"));
    }

    /// Golden test: a fixed set of inputs must always produce this exact
    /// text. `plugin/src/ui/ui-logic.test.ts` has a matching test with the
    /// same inputs and the same expected string, so the 2 implementations
    /// are checked against one shared fixture rather than against each
    /// other directly.
    #[test]
    fn golden_matches_the_fixed_expected_text_for_each_file_count() {
        let zero = fill_agent_prompt(TEMPLATE, &[], "/tmp/bridge", 18846);
        assert_eq!(
            zero,
            "turbofig file-bridge: write {\"op\":\"execute\",\"fileKey\":\"<fileKey>\",\"code\":\"...\"} to /tmp/bridge/inbox/<unique-id>.json (id unique per job) -> read /tmp/bridge/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. Run the status op first to learn the fileKey. If no result file appears within a few seconds, run `turbofig start` once, then retry. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:18846/mcp\n"
        );

        let one = fill_agent_prompt(
            TEMPLATE,
            &[file("ABC123", "Design A")],
            "/tmp/bridge",
            18846,
        );
        assert_eq!(
            one,
            "turbofig file-bridge: write {\"op\":\"execute\",\"fileKey\":\"ABC123\",\"code\":\"...\"} to /tmp/bridge/inbox/<unique-id>.json (id unique per job) -> read /tmp/bridge/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. If no result file appears within a few seconds, run `turbofig start` once, then retry. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:18846/mcp\n"
        );

        let many = fill_agent_prompt(
            TEMPLATE,
            &[file("key1", "Design A"), file("key2", "Design B")],
            "/tmp/bridge",
            18846,
        );
        assert_eq!(
            many,
            "Connected files: Design A (key1), Design B (key2).\nturbofig file-bridge: write {\"op\":\"execute\",\"fileKey\":\"<fileKey>\",\"code\":\"...\"} to /tmp/bridge/inbox/<unique-id>.json (id unique per job) -> read /tmp/bridge/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. Pick a fileKey from the list above. If no result file appears within a few seconds, run `turbofig start` once, then retry. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:18846/mcp\n"
        );
    }
}
