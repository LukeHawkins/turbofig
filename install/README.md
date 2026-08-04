# turbofig macOS install

These files install the turbofig daemon as a macOS LaunchAgent. The daemon starts at login and restarts automatically on crash.

## Requirements

- macOS (tested on macOS 14+)
- A built `turbofig-mcp` binary, OR `cargo` in `PATH` (the script builds it)

## Install

Run the install script from the repository root or any directory:

```sh
./install/install-macos.sh
```

The script does the following:

1. Finds or builds the `turbofig-mcp` release binary.
2. Creates `~/Library/Logs/turbofig/` for log files.
3. Writes the LaunchAgent plist to `~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist`.
4. Loads the service with `launchctl load -w`.

The install is idempotent. Run it again to reinstall or to pick up a new binary path.

### Override the binary path

Set `TURBOFIG_BIN` to use a specific binary:

```sh
TURBOFIG_BIN=/usr/local/bin/turbofig-mcp ./install/install-macos.sh
```

## Check status

```sh
launchctl list | grep turbofig
```

A non-zero PID in the first column means the daemon is running.

## Log locations

| File | Content |
|---|---|
| `~/Library/Logs/turbofig/turbofig.out.log` | Standard output |
| `~/Library/Logs/turbofig/turbofig.err.log` | Standard error |

Stream logs live:

```sh
tail -f ~/Library/Logs/turbofig/turbofig.out.log
tail -f ~/Library/Logs/turbofig/turbofig.err.log
```

## Ports

The daemon uses these ports by default:

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | 18846 | HTTP MCP endpoint |
| `TURBOFIG_WS_PORT` | 18847 | WebSocket for the Figma plugin |

Edit the `EnvironmentVariables` section in the installed plist to change the ports. Reload the service after editing:

```sh
launchctl unload ~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist
launchctl load -w ~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist
```

## Uninstall

```sh
./install/install-macos.sh --uninstall
```

This unloads the service and removes the plist. Log files remain at `~/Library/Logs/turbofig/`. Remove them manually if you do not need them.
