//! Writes the embedded Figma plugin out to `<home>/figma-plugin/`.
//!
//! Given a home directory and the daemon's pairing token, write
//! `manifest.json`, `code.js`, and a `ui.html` with the real token injected
//! in place of the `__TURBOFIG_PAIRING_TOKEN__` placeholder the embedded
//! build carries (see `embedded.rs` and `plugin/build-ui.ts`). The daemon's
//! own startup (`main.rs`'s `run_daemon`) calls this, not a separate
//! install-time command.

use crate::embedded::embedded_plugin;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Placeholder text the embedded `ui.html` carries in place of a real token.
/// Must match the literal `plugin/build-ui.ts` emits by default.
const TOKEN_PLACEHOLDER: &str = "__TURBOFIG_PAIRING_TOKEN__";

/// Name of the marker file written alongside the plugin files. Records the
/// daemon version and a hash of the token that was last written, so
/// `plugin_files_outdated` can tell a stale copy from a current one without
/// ever storing the token itself on disk a second time.
const MARKER_FILE: &str = ".turbofig-version";

/// Writes the embedded plugin's three files into `<home>/figma-plugin/`,
/// with `token` injected into `ui.html`, and a version marker.
///
/// Returns the path to the written `manifest.json` (the file Figma's
/// "Import plugin from manifest" dialog points at).
///
/// Writes are atomic (temp file in the same directory, then rename) and the
/// directory and every file inside it are owner-only (0700 / 0600): the
/// marker records a hash of the token, and `ui.html` carries the token
/// itself, so a group- or world-readable folder would leak it to another
/// local user.
pub fn write_plugin_files(home: &Path, token: &str) -> io::Result<PathBuf> {
    // Refuse a symlinked or foreign-owned plugin folder before anything is
    // written: the files embed the token.
    let refused_dir = home.join("figma-plugin");
    crate::secure_fs::check_existing(&refused_dir)?;
    let plugin = embedded_plugin().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "this build has no embedded plugin; build plugin/dist first (cd plugin && bun run build), then rebuild the daemon",
        )
    })?;

    let dir = home.join("figma-plugin");
    let dist_dir = dir.join("dist");
    std::fs::create_dir_all(&dist_dir)?;
    crate::secure_fs::check_existing(&dir)?;
    crate::secure_fs::check_existing(&dist_dir)?;
    set_owner_only(&dir)?;
    set_owner_only(&dist_dir)?;

    let manifest_path = dir.join("manifest.json");
    write_atomic_0600(&manifest_path, plugin.manifest.as_bytes())?;
    write_atomic_0600(&dist_dir.join("code.js"), plugin.code_js.as_bytes())?;

    let ui_html = plugin.ui_html.replace(TOKEN_PLACEHOLDER, token);
    write_atomic_0600(&dist_dir.join("ui.html"), ui_html.as_bytes())?;

    write_atomic_0600(&dir.join(MARKER_FILE), marker_contents(token).as_bytes())?;

    // Old daemon versions wrote code.js and ui.html at the root of
    // figma-plugin/, which did not match the manifest's dist/ paths. Remove
    // any leftover root-level copies on refresh so a stale file never shadows
    // the correct one.
    for stale in ["code.js", "ui.html"] {
        let stale_path = dir.join(stale);
        if stale_path.exists() {
            std::fs::remove_file(&stale_path)?;
        }
    }

    Ok(manifest_path)
}

/// Returns true when the plugin files at `<home>/figma-plugin/` are missing,
/// unreadable, stamped with a different daemon version or a different token
/// than the ones given, or incomplete: a missing `dist/code.js` or
/// `dist/ui.html`, or a leftover root-level `code.js`/`ui.html` from an old
/// daemon version that wrote them there instead of under `dist/`. A caller
/// (the daemon's own startup) uses this to decide whether to call
/// `write_plugin_files` again, so a plugin reload is only needed after a real
/// daemon upgrade, a token rotation, or a layout the marker alone cannot see.
pub fn plugin_files_outdated(home: &Path, token: &str) -> bool {
    let dir = home.join("figma-plugin");
    let marker_path = dir.join(MARKER_FILE);
    match std::fs::read_to_string(&marker_path) {
        Ok(contents) if contents == marker_contents(token) => {
            !dir.join("dist/code.js").exists()
                || !dir.join("dist/ui.html").exists()
                || dir.join("code.js").exists()
                || dir.join("ui.html").exists()
        }
        _ => true,
    }
}

/// Builds the marker file contents for `token`: the daemon version on one
/// line, a hash of the token on the next. Never writes the token itself.
fn marker_contents(token: &str) -> String {
    format!(
        "{}\n{:016x}\n",
        env!("CARGO_PKG_VERSION"),
        hash_token(token)
    )
}

/// A stable (same-process-build), non-cryptographic hash of the token, used
/// only to detect "the token changed since the marker was written". This is
/// a staleness check, not a security boundary: the real token lives only in
/// `<home>/token` (owner-only) and inside `ui.html` (also owner-only).
fn hash_token(token: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

/// Writes `contents` to `path` atomically: a sibling `.tmp` file, flushed and
/// closed, set to mode 0600, then renamed over `path`. The rename is allowed
/// to replace an existing file here (unlike the pairing token itself): a
/// plugin file is meant to be refreshed on every daemon upgrade.
fn write_atomic_0600(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp_path)?;
        file.write_all(contents)?;
    }
    set_owner_only(&tmp_path)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Name of the marker file the daemon writes at `<home>/plugin-seen` the
/// first time a plugin connects with a valid pairing token (see `ws.rs`).
/// Its presence is what `turbofig` (the bare, no-argument command) uses to
/// tell a genuine first run from a later one: a first run prints the full
/// "add the plugin" walkthrough, a later one prints a short status instead.
const PLUGIN_SEEN_FILE: &str = "plugin-seen";

/// Writes `<home>/plugin-seen` (mode 0600, holding the current Unix time in
/// seconds) if it does not already exist. A no-op, returning `Ok(false)`,
/// when the marker is already there: this runs on every successful plugin
/// WebSocket authentication, not only the first, so it must never overwrite
/// an existing marker or treat a second call as an error. Returns `Ok(true)`
/// when this call is the one that created it.
pub fn mark_plugin_seen(home: &Path) -> io::Result<bool> {
    let path = home.join(PLUGIN_SEEN_FILE);
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(home)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    write_atomic_0600(&path, format!("{now}\n").as_bytes())?;
    Ok(true)
}

/// Marker file in the turbofig home. Its presence means "MCP" mode; its
/// absence means "File bridge" mode (the default). The daemon is the single
/// writer while it runs.
pub(crate) const MCP_MODE_FILE: &str = "mcp-mode";

/// The old marker of the 0.2.0 development builds. Its presence meant "MCP
/// over HTTP off", which equals the new default, so startup deletes it.
pub(crate) const LEGACY_MCP_HTTP_OFF_FILE: &str = "mcp-http-off";

/// The shared "Connection" setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    /// The agent runs `turbofig` in its shell. The default.
    File,
    /// The daemon serves MCP over HTTP.
    Mcp,
}

impl ConnectionMode {
    /// The wire value: `"file"` or `"mcp"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionMode::File => "file",
            ConnectionMode::Mcp => "mcp",
        }
    }

    /// Parses the wire value. Anything else is `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(ConnectionMode::File),
            "mcp" => Some(ConnectionMode::Mcp),
            _ => None,
        }
    }
}

/// The state of the setting, read from the marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionModeState {
    /// No marker (the default): File bridge.
    File,
    /// A regular marker file owned by this user: MCP.
    Mcp,
    /// The marker is something else (symlink, directory, foreign owner) or
    /// could not be read. The reason is for the log and the Settings window.
    Unknown(String),
}

/// Reads the setting with one `lstat`: cheap enough for every `/mcp` request.
/// Only `NotFound` means File bridge. Only a regular file owned by this user
/// means MCP. Anything else is `Unknown`.
pub(crate) fn connection_mode_state(home: &Path) -> ConnectionModeState {
    let path = home.join(MCP_MODE_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return ConnectionModeState::File,
        Err(e) => {
            return ConnectionModeState::Unknown(format!("cannot read {}: {e}", path.display()))
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if meta.uid() != euid {
            return ConnectionModeState::Unknown(format!(
                "{} is owned by uid {}, not by this user",
                path.display(),
                meta.uid()
            ));
        }
    }
    if !meta.file_type().is_file() {
        return ConnectionModeState::Unknown(format!(
            "{} is not a regular file (symlink or directory); delete it",
            path.display()
        ));
    }
    ConnectionModeState::Mcp
}

/// The effective mode. An `Unknown` marker counts as File bridge (fail
/// closed: `/mcp` is a capability, so an odd marker must not open it). The
/// reason goes to stderr.
pub(crate) fn connection_mode(home: &Path) -> ConnectionMode {
    match connection_mode_state(home) {
        ConnectionModeState::File => ConnectionMode::File,
        ConnectionModeState::Mcp => ConnectionMode::Mcp,
        ConnectionModeState::Unknown(why) => {
            eprintln!("turbofig: Connection treated as File bridge: {why}");
            ConnectionMode::File
        }
    }
}

/// Sets the Connection: MCP writes the marker (mode 0600), File bridge
/// removes it. A no-op when the state already matches. Refuses to touch a
/// marker that is not a regular file owned by this user. The home goes
/// through `secure_fs::ensure_private_dir`. Never follows a symlink.
pub(crate) fn set_connection_mode(home: &Path, mode: ConnectionMode) -> io::Result<()> {
    crate::secure_fs::ensure_private_dir(home, false)?;
    let path = home.join(MCP_MODE_FILE);
    match (connection_mode_state(home), mode) {
        (ConnectionModeState::Unknown(why), _) => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, why));
        }
        (ConnectionModeState::File, ConnectionMode::File)
        | (ConnectionModeState::Mcp, ConnectionMode::Mcp) => return Ok(()),
        (ConnectionModeState::Mcp, ConnectionMode::File) => return std::fs::remove_file(&path),
        (ConnectionModeState::File, ConnectionMode::Mcp) => {}
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);
    // A stale or planted `.tmp` is removed (a symlink is unlinked, never
    // followed), then the file is created fresh.
    match std::fs::remove_file(&tmp_path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = opts.open(&tmp_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(b"mcp\n")?;
    }
    std::fs::rename(&tmp_path, &path)
}

/// Deletes a leftover `mcp-http-off` marker (a file or a symlink, never a
/// directory). The old marker meant "off", which equals the new default.
/// Best effort: a failure is logged and ignored.
pub(crate) fn remove_legacy_mcp_http_off(home: &Path) {
    let path = home.join(LEGACY_MCP_HTTP_OFF_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.file_type().is_dir() => {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("turbofig: could not remove {}: {e}", path.display());
            }
        }
        _ => {}
    }
}

/// Sets `path` (a file or directory) to mode 0700 and masks group/world bits
/// for a file; see `bridge::set_owner_only` for the same reasoning applied to
/// the file-bridge's inbox/outbox. A failure here is not swallowed, unlike
/// the bridge's best-effort hardening: these files carry the pairing token
/// (`ui.html`) or a hash of it (the marker), so a failed chmod must surface
/// as a write failure instead of silently leaving a readable file behind.
#[cfg(unix)]
fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_plugin_files_writes_all_three_files_plus_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path =
            write_plugin_files(tmp.path(), "test-token-abc").expect("write_plugin_files");

        let dir = tmp.path().join("figma-plugin");
        assert_eq!(manifest_path, dir.join("manifest.json"));
        assert!(dir.join("manifest.json").exists());
        assert!(dir.join("dist/code.js").exists());
        assert!(dir.join("dist/ui.html").exists());
        assert!(dir.join(MARKER_FILE).exists());
    }

    #[test]
    fn write_plugin_files_injects_the_token_into_ui_html() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "my-secret-token").expect("write_plugin_files");
        let ui_html = std::fs::read_to_string(tmp.path().join("figma-plugin/dist/ui.html"))
            .expect("read ui.html");
        assert!(
            ui_html.contains("my-secret-token"),
            "ui.html must carry the real token"
        );
        assert!(
            !ui_html.contains(TOKEN_PLACEHOLDER),
            "the placeholder must be fully replaced"
        );
    }

    #[test]
    fn write_plugin_files_sets_owner_only_modes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok").expect("write_plugin_files");
        let dir = tmp.path().join("figma-plugin");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir)
                .expect("stat dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            for name in ["manifest.json", "dist/code.js", "dist/ui.html", MARKER_FILE] {
                let mode = std::fs::metadata(dir.join(name))
                    .unwrap_or_else(|_| panic!("stat {name}"))
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "{name} must be mode 0600");
            }
        }
    }

    #[test]
    fn write_plugin_files_manifest_paths_resolve_on_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path =
            write_plugin_files(tmp.path(), "test-token-abc").expect("write_plugin_files");
        let dir = manifest_path.parent().expect("manifest has a parent dir");

        let manifest_text = std::fs::read_to_string(&manifest_path).expect("read manifest");
        let manifest: serde_json::Value =
            serde_json::from_str(&manifest_text).expect("manifest is valid JSON");

        for key in ["main", "ui"] {
            let rel_path = manifest[key]
                .as_str()
                .unwrap_or_else(|| panic!("manifest.{key} must be a string"));
            assert!(
                dir.join(rel_path).exists(),
                "manifest.{key} names {rel_path}, which must exist under {dir:?}"
            );
        }
    }

    #[test]
    fn plugin_files_outdated_is_true_before_any_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(plugin_files_outdated(tmp.path(), "tok"));
    }

    #[test]
    fn plugin_files_outdated_is_false_right_after_a_matching_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok").expect("write_plugin_files");
        assert!(!plugin_files_outdated(tmp.path(), "tok"));
    }

    #[test]
    fn plugin_files_outdated_is_true_after_the_token_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok-a").expect("write_plugin_files");
        assert!(plugin_files_outdated(tmp.path(), "tok-b"));
    }

    #[test]
    fn plugin_files_outdated_is_true_when_dist_code_js_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok").expect("write_plugin_files");
        std::fs::remove_file(tmp.path().join("figma-plugin/dist/code.js"))
            .expect("remove dist/code.js");
        assert!(
            plugin_files_outdated(tmp.path(), "tok"),
            "a missing dist/code.js must count as outdated even with a matching marker"
        );
    }

    #[test]
    fn plugin_files_outdated_is_true_when_dist_ui_html_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok").expect("write_plugin_files");
        std::fs::remove_file(tmp.path().join("figma-plugin/dist/ui.html"))
            .expect("remove dist/ui.html");
        assert!(
            plugin_files_outdated(tmp.path(), "tok"),
            "a missing dist/ui.html must count as outdated even with a matching marker"
        );
    }

    #[test]
    fn plugin_files_outdated_is_true_for_a_leftover_root_layout_install() {
        // An old daemon version with the same recorded version and token
        // wrote code.js/ui.html at the root of figma-plugin/ instead of
        // dist/. The marker alone cannot see this; the leftover root files
        // must force an outdated result so write_plugin_files runs again
        // and cleans them up.
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok").expect("write_plugin_files");
        let dir = tmp.path().join("figma-plugin");
        std::fs::write(dir.join("code.js"), b"stale root code.js").expect("write stale code.js");
        assert!(
            plugin_files_outdated(tmp.path(), "tok"),
            "a leftover root-level code.js must count as outdated"
        );

        std::fs::remove_file(dir.join("code.js")).expect("remove stale code.js");
        std::fs::write(dir.join("ui.html"), b"stale root ui.html").expect("write stale ui.html");
        assert!(
            plugin_files_outdated(tmp.path(), "tok"),
            "a leftover root-level ui.html must count as outdated"
        );
    }

    #[test]
    fn connection_is_file_bridge_by_default_and_the_marker_toggles_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(connection_mode(tmp.path()), ConnectionMode::File);
        assert_eq!(connection_mode_state(tmp.path()), ConnectionModeState::File);
        set_connection_mode(tmp.path(), ConnectionMode::Mcp).expect("mcp");
        assert_eq!(connection_mode(tmp.path()), ConnectionMode::Mcp);
        assert_eq!(connection_mode_state(tmp.path()), ConnectionModeState::Mcp);
        set_connection_mode(tmp.path(), ConnectionMode::Mcp).expect("mcp twice");
        set_connection_mode(tmp.path(), ConnectionMode::File).expect("file");
        assert_eq!(connection_mode(tmp.path()), ConnectionMode::File);
        assert!(!tmp.path().join(MCP_MODE_FILE).exists());
        set_connection_mode(tmp.path(), ConnectionMode::File).expect("file twice");
    }

    #[test]
    fn connection_mode_parses_only_the_two_wire_values() {
        assert_eq!(ConnectionMode::parse("file"), Some(ConnectionMode::File));
        assert_eq!(ConnectionMode::parse("mcp"), Some(ConnectionMode::Mcp));
        assert_eq!(ConnectionMode::parse("MCP"), None);
        assert_eq!(ConnectionMode::parse(""), None);
        assert_eq!(ConnectionMode::File.as_str(), "file");
        assert_eq!(ConnectionMode::Mcp.as_str(), "mcp");
    }

    #[test]
    fn the_legacy_marker_is_removed_and_a_directory_is_left_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join(LEGACY_MCP_HTTP_OFF_FILE);
        std::fs::write(&legacy, "off\n").expect("write legacy");
        remove_legacy_mcp_http_off(tmp.path());
        assert!(!legacy.exists(), "the legacy marker must be deleted");
        assert_eq!(connection_mode(tmp.path()), ConnectionMode::File);
        std::fs::create_dir(&legacy).expect("mkdir");
        remove_legacy_mcp_http_off(tmp.path());
        assert!(legacy.is_dir(), "a directory must not be removed");
        remove_legacy_mcp_http_off(&tmp.path().join("nope"));
    }

    #[cfg(unix)]
    #[test]
    fn the_mcp_mode_marker_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        set_connection_mode(tmp.path(), ConnectionMode::Mcp).expect("mcp");
        let mode = std::fs::metadata(tmp.path().join(MCP_MODE_FILE))
            .expect("stat marker")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_tmp_is_not_followed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("victim");
        std::fs::write(&target, "keep me").expect("write victim");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let tmp_marker = tmp.path().join(format!("{MCP_MODE_FILE}.tmp"));
        std::os::unix::fs::symlink(&target, &tmp_marker).expect("symlink");
        set_connection_mode(tmp.path(), ConnectionMode::Mcp).expect("mcp");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the symlink target must not be chmodded");
        assert_eq!(connection_mode_state(tmp.path()), ConnectionModeState::Mcp);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_marker_is_unknown_fails_closed_and_is_not_changed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("elsewhere");
        std::fs::write(&target, "x").expect("write");
        let marker = tmp.path().join(MCP_MODE_FILE);
        std::os::unix::fs::symlink(&target, &marker).expect("symlink");
        assert!(matches!(
            connection_mode_state(tmp.path()),
            ConnectionModeState::Unknown(_)
        ));
        assert_eq!(connection_mode(tmp.path()), ConnectionMode::File);
        assert!(set_connection_mode(tmp.path(), ConnectionMode::Mcp).is_err());
        assert!(set_connection_mode(tmp.path(), ConnectionMode::File).is_err());
        assert!(std::fs::symlink_metadata(&marker)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn a_directory_marker_is_unknown_fails_closed_and_errors_on_set() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(MCP_MODE_FILE)).expect("mkdir");
        assert!(matches!(
            connection_mode_state(tmp.path()),
            ConnectionModeState::Unknown(_)
        ));
        assert_eq!(connection_mode(tmp.path()), ConnectionMode::File);
        assert!(set_connection_mode(tmp.path(), ConnectionMode::Mcp).is_err());
    }

    #[test]
    fn mark_plugin_seen_creates_the_marker_with_mode_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let wrote = mark_plugin_seen(tmp.path()).expect("mark_plugin_seen");
        assert!(wrote, "the first call must report that it wrote the marker");
        let path = tmp.path().join(PLUGIN_SEEN_FILE);
        assert!(path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("stat marker")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let contents = std::fs::read_to_string(&path).expect("read marker");
        assert!(
            contents.trim().parse::<u64>().is_ok(),
            "the marker must hold a numeric timestamp: {contents:?}"
        );
    }

    #[test]
    fn mark_plugin_seen_never_overwrites_an_existing_marker() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(mark_plugin_seen(tmp.path()).expect("first call"));
        let path = tmp.path().join(PLUGIN_SEEN_FILE);
        let original = std::fs::read_to_string(&path).expect("read marker");

        let wrote_again = mark_plugin_seen(tmp.path()).expect("second call");
        assert!(
            !wrote_again,
            "a second call must report it did not write a new marker"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read marker again"),
            original,
            "an existing marker must never be overwritten"
        );
    }

    #[test]
    fn write_plugin_files_is_idempotent_and_overwrites_a_previous_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_plugin_files(tmp.path(), "tok-a").expect("first write");
        write_plugin_files(tmp.path(), "tok-b").expect("second write");
        let ui_html = std::fs::read_to_string(tmp.path().join("figma-plugin/dist/ui.html"))
            .expect("read ui.html");
        assert!(ui_html.contains("tok-b"));
        assert!(!ui_html.contains("tok-a"));
        assert!(!plugin_files_outdated(tmp.path(), "tok-b"));
    }

    #[cfg(unix)]
    #[test]
    fn write_plugin_files_refuses_a_symlinked_plugin_dir_and_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("elsewhere");
        std::fs::create_dir(&target).expect("mkdir target");
        std::os::unix::fs::symlink(&target, tmp.path().join("figma-plugin")).expect("symlink");

        let err = write_plugin_files(tmp.path(), "secret-token")
            .expect_err("a symlinked figma-plugin must be refused");
        assert!(err.to_string().contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read_dir(&target).unwrap().count(),
            0,
            "no file may be written into the refused folder"
        );
    }
}
