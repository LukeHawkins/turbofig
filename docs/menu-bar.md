# Menu bar

`turbofig.app` is assembled on your own Mac (not downloaded as a `.app`),
so it carries no quarantine flag and opens with no "unidentified
developer" prompt, even though the `turbofig` binary itself is not
notarized. It shows as a tf icon in your menu bar, dimmed when no Figma
file is connected or the daemon is unreachable, and solid once a file
connects. Its menu:

| Item | Does |
|---|---|
| About turbofig… | Opens the About window: the "How to use" steps |
| Settings… | Opens the Settings window: Start at login, Connection (File bridge or MCP), Copy path, Show plugin folder |
| Quit turbofig | Stops the daemon and quits the app |

The menu is kept small on purpose: the connect prompt is copied from the
Figma plugin (the link icon next to the file name, "Copy connect prompt"); copying the
manifest path, revealing the plugin in Finder, Start at login and Connection
live in the Settings window. Both windows are plain
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

A running daemon notices the upgrade by itself, within about 30 seconds. It
stops taking new jobs, lets its in-flight jobs finish, then restarts in
place on the new build. You do not need to stop it. The daemon rewrites the
plugin files and refreshes `turbofig.app` when it restarts, so the Figma
import only happens once: reopen the plugin in Figma after an upgrade to
pick up the refreshed files (a plugin you left open shows a "Reload the
plugin" notice). If the app was already open, it notices that its bundle
changed and relaunches itself, automatically, to pick up the refreshed
bundle. This also works for a rebuild with the same version.

If a daemon from an older turbofig is still running (one that cannot restart
itself), opening the app, running `turbofig`, running `turbofig mcp` or
running `turbofig js` restarts it once on the new version. An older proxy
never restarts a newer daemon.

**Uninstalling:** run `turbofig uninstall` before `brew uninstall
turbofig`. It quits a running app first (if any), stops the running
daemon, turns off autostart, and removes the app bundle.
