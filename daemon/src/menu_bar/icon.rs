//! The tray icon's 2 states: a placeholder "tf" glyph, black on transparent,
//! loaded as a macOS template image (see `daemon/assets/tray-icon/README.md`).
//! Decoding (PNG -> RGBA) happens here so `mod.rs` only ever asks for a
//! ready-to-use `tray_icon::Icon`.

use super::state::IconState;

/// The "a plugin is connected" icon: full opacity.
const NORMAL_PNG: &[u8] = include_bytes!("../../assets/tray-icon/icon-tf-44.png");
/// The "no file connected, or the daemon is unreachable" icon: lower alpha.
const DIMMED_PNG: &[u8] = include_bytes!("../../assets/tray-icon/icon-tf-44-dimmed.png");

/// Decodes `png_bytes` into a `tray_icon::Icon`. Both PNGs are checked-in,
/// fixed, well-formed assets (see the README next to them), so a decode
/// failure here can only mean a corrupt build artifact; this still reports
/// `None` rather than panicking, so a bad icon never takes the whole app
/// down. The caller falls back to `TrayIconBuilder` with no icon at all
/// (an empty menu-bar slot is recoverable; a crashed menu bar is not).
fn decode_icon(png_bytes: &[u8]) -> Option<tray_icon::Icon> {
    let image = image::load_from_memory(png_bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    tray_icon::Icon::from_rgba(image.into_raw(), width, height).ok()
}

/// Loads the icon for `state`. `None` only on a decode failure (see
/// `decode_icon`); the caller must tolerate a tray icon with no image rather
/// than treat this as fatal.
pub fn icon_for_state(state: IconState) -> Option<tray_icon::Icon> {
    match state {
        IconState::Normal => decode_icon(NORMAL_PNG),
        IconState::Dimmed => decode_icon(DIMMED_PNG),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_checked_in_icons_decode() {
        assert!(icon_for_state(IconState::Normal).is_some());
        assert!(icon_for_state(IconState::Dimmed).is_some());
    }
}
