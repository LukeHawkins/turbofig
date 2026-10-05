//! Writes the embedded Figma plugin out to `<home>/figma-plugin/`.
//!
//! This is the half of `turbofig setup` (a later command, not built here)
//! that touches disk: given a home directory and the daemon's pairing token,
//! write `manifest.json`, `code.js`, and a `ui.html` with the real token
//! injected in place of the `__TURBOFIG_PAIRING_TOKEN__` placeholder the
//! embedded build carries (see `embedded.rs` and `plugin/build-ui.ts`).

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
    let plugin = embedded_plugin().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "this build has no embedded plugin; build plugin/dist first (cd plugin && bun run build), then rebuild the daemon",
        )
    })?;

    let dir = home.join("figma-plugin");
    let dist_dir = dir.join("dist");
    std::fs::create_dir_all(&dist_dir)?;
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
/// unreadable, or stamped with a different daemon version or a different
/// token than the ones given. A caller (the future `turbofig setup`) uses
/// this to decide whether to call `write_plugin_files` again, so a plugin
/// reload is only needed after a real daemon upgrade or a token rotation.
pub fn plugin_files_outdated(home: &Path, token: &str) -> bool {
    let marker_path = home.join("figma-plugin").join(MARKER_FILE);
    match std::fs::read_to_string(&marker_path) {
        Ok(contents) => contents != marker_contents(token),
        Err(_) => true,
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
}
