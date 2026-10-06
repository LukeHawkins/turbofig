//! Activates the app and brings a just-opened (or refocused) window to the
//! front. The app runs under `ActivationPolicy::Accessory` (no Dock icon),
//! so a plain `window.set_focus()` alone can leave the window behind other
//! apps and, worse, leave it non-key: a non-key `WKWebView` does not pass
//! its first click through to the page, it only focuses the window. This is
//! the real cause of a tab click (or any other first click) doing nothing
//! the first time a menu-bar window opens. Calling `activateIgnoringOtherApps`
//! before focusing the window fixes both the ordering (app comes to front)
//! and the key-window state (clicks land on the first try).

/// Activates this process (brings it and its windows to the front, ignoring
/// whichever app was frontmost), then makes `window` key and visible. Call
/// this whenever a menu-bar window opens or is refocused.
pub fn activate_app_and_focus(window: &tao::window::Window) {
    #[cfg(target_os = "macos")]
    {
        let mtm = unsafe { objc2::MainThreadMarker::new_unchecked() };
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        app.activate();
    }
    window.set_visible(true);
    window.set_focus();
}
