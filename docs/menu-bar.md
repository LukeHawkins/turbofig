# Menu bar

`turbofig.app` is assembled on your own Mac (not downloaded as a `.app`),
so it carries no quarantine flag and opens with no "unidentified
developer" prompt, even though the `turbofig` binary itself is not
notarized. It shows as a tf icon in your menu bar, dimmed when no Figma
file is connected or the daemon is unreachable, and solid once a file
connects. Its menu:

| Item | Does |
|---|---|
| About turbofig… | Opens the About window: the "How to use" steps and Copy agent prompt |
| Settings… | Opens the Settings window: Start at login, Copy plugin manifest path, Open plugin folder |
| Quit turbofig | Stops the daemon and quits the app |

The menu is kept small on purpose: copying the agent prompt lives in the
About window; copying the manifest path, revealing the plugin in Finder,
and Start at login live in the Settings window. Both windows are plain
pages with no script: every button is a link Rust intercepts and runs, so
there is no live status display to go stale; close either window with its
title bar button or Cmd+W.

**Start at Login:** `turbofig autostart on` (the default) installs a
LaunchAgent that launches `turbofig.app` itself at login; `turbofig
autostart on --headless` installs a daemon-only LaunchAgent instead, with
no app, no tray icon, no window, for a machine where you only want the
background service. `turbofig autostart off` removes whichever one is
installed. Turning one on always replaces the other, so the 2 never run
at once. The Settings window's "Start at login" link is the one view onto
this setting now; it shows the current state as plain text and reopen the
window to see the setting change take effect.

**Updating:**

```bash
brew upgrade turbofig
```

The next `turbofig mcp` start (or the next time you open the app) sees an
older daemon, lets its in-flight jobs finish, then restarts it on the new
version. An older proxy never restarts a newer daemon. The daemon
rewrites the plugin files and refreshes `turbofig.app` on every start, so
the Figma import only happens once: reopen the plugin in Figma after an
upgrade to pick up the refreshed files. If the app was already open when
you upgraded, it notices the daemon is now newer and relaunches itself
once, automatically, to pick up the refreshed bundle; it never relaunches
twice for the same upgrade.

**Uninstalling:** run `turbofig uninstall` before `brew uninstall
turbofig`. It quits a running app first (if any), stops the running
daemon, turns off autostart, and removes the app bundle.
