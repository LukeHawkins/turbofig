# Commands

| Command | Does |
|---|---|
| `turbofig` | Installs/refreshes `turbofig.app`, starts the daemon, opens the app, and prints a 2-line pointer at the tray icon. On any other OS, or if the app could not be opened, prints the 3 connect steps on first run, or a 3-line status on a later run, instead |
| `turbofig mcp` | Runs a stdio MCP server for an agent's MCP client. Starts the daemon first if it is not already running |
| `turbofig js [--file <fileKey>] [--shot] [--dir <folder>]` | Reads JS code from stdin (or a full job JSON with an `op` field), sends it through the file bridge, waits for the result and prints the result JSON. `--shot` also screenshots the first node id the code returned and prints that result as a second line. `--dir` names the bridge folder (it wins over `TURBOFIG_BRIDGE_DIR`; a relative path is made absolute). Starts the daemon first if none runs. Exit 0: a result printed, even `"ok":false`. Exit 1: no result, with one JSON line on stdout: `not_started` (the job never ran: resend), `started_unknown` (the daemon claimed it and it may have run: check before resending), or `bridge_error` (the folder is unusable, or the job could not be written or the result read). Exit 2: usage error, message on stderr (also: no input on stdin within 10 s). With `--shot`, exit 1 when the screenshot job got no result; exit 0 when it ran and the plugin reported an error |
| `turbofig start` | Starts the daemon detached in the background, if it is not already running |
| `turbofig stop` | Stops the running daemon |
| `turbofig status` | Queries the running daemon's `/health` endpoint and prints a readable report |
| `turbofig serve` | Runs the daemon in the foreground. For development, or for an autostart launchd service |
| `turbofig autostart on [--headless]\|off` | Turns on or off the launchd service that starts the app (or, with `--headless`, just the daemon) at login |
| `turbofig uninstall [--purge]` | Quits a running app, stops the daemon, turns off autostart, and removes the app bundle and its plist(s). With `--purge`, also removes the turbofig home folder's own files (the token, the plugin files, the inbox, the outbox, the log), and removes the folder itself only if it is then empty |

See [configuration.md](configuration.md) for the environment variables
these commands read.
