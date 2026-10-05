<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/wordmark-light.png">
    <img alt="turbofig wordmark" src="docs/wordmark-light.png" height="80">
  </picture>

  <p>Bridge any AI to Figma.</p>

  [![CI](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml/badge.svg)](https://github.com/LukeHawkins/turbofig/actions/workflows/ci.yml)
  [![Release](https://img.shields.io/github/v/release/LukeHawkins/turbofig)](https://github.com/LukeHawkins/turbofig/releases)
  [![License: MIT](https://img.shields.io/github/license/LukeHawkins/turbofig)](LICENSE)
  [![Platform: macOS](https://img.shields.io/badge/platform-macOS-lightgrey)](#)
</div>

---

Turbofig is a local, always-on daemon that lets any AI agent read and edit
the Figma file open in Figma Desktop. It talks to a thin Figma plugin over
a WebSocket and exposes 4 tools to the agent, including one tool that runs
Figma Plugin API JavaScript directly. The daemon runs as a launchd service,
so it keeps running after your agent session ends.

**Works with:**

- **Platform:** macOS (Apple Silicon and Intel). Windows and Linux are not
  supported yet.
- **Agents:** any local agent that can read and write files (file bridge),
  or any MCP client with streamable HTTP, for example Claude Code.
- Chat apps that run in a browser cannot reach your Mac, so they cannot use
  turbofig.

## Why turbofig

![How turbofig works](docs/how-it-works.png)
*The daemon bridges your AI agent to the Figma plugin over a local
WebSocket.*

- **4 tools, so a small schema.** All capability flows through
  `turbofig_execute`. The tool surface is locked at 4 and never grows.
- **A single binary with no Node runtime.** The daemon is one Rust binary.
  It embeds the Figma plugin and writes it to disk on `turbofig setup`.
- **No Figma access token.** Turbofig drives the file through the Figma
  Plugin API, not the Figma web API, so it never needs a personal access
  token.
- **The daemon keeps running when an agent session ends.** It is a launchd
  service with `KeepAlive`, decoupled from any one client session.
- **The file bridge works where adding MCP servers is blocked by policy.**
  An agent drives the daemon with file writes and reads only: no curl, no
  MCP connection.
- **Localhost only, with Origin checks and a pairing token.** Both ports
  bind to `127.0.0.1`. See [Security](#security).

### turbofig vs figma-console-mcp

Countable facts, not an opinion. figma-console-mcp facts are from its npm
package, version 1.22.1.

| | turbofig | figma-console-mcp 1.22.1 |
|---|---|---|
| Tools | 4 | 46 registered in its local-mode source (its own README advertises "94+" across all its modes; this count covers only the local-mode tool list) |
| Figma access token needed | No | Yes, a personal access token |
| Runtime | Single Rust binary, no Node | Node.js, run through `npx` |
| Figma Desktop must be open | Yes | Yes, for its Desktop Bridge plugin features |
| REST-only features (comments, Code Connect, reading a file without Figma open) | No. Turbofig has no REST access | Comments are supported through the Figma REST API. Code Connect support is not confirmed |

**When to use something else.** If you need comments, Code Connect, or file
access without Figma open, use a REST-based server such as
figma-console-mcp or Figma's own MCP server. This README makes no claim
about Figma's own MCP server beyond that it exists.

## Quickstart

```bash
brew install LukeHawkins/tap/turbofig
turbofig setup
```

`turbofig setup` installs the pairing token, writes the Figma plugin files,
and starts the daemon at login. It then prints 3 steps:

1. In Figma Desktop: **Plugins > Development > Import plugin from
   manifest**, then pick the printed manifest path. This menu item exists
   only in Figma Desktop, not the web app. Figma's file picker hides
   `~/.turbofig` by default. `turbofig setup` puts the manifest path on
   your clipboard, so press Cmd+Shift+G in the picker and paste it in.
2. Run the turbofig plugin in a file.
3. Click the copy-prompt button in the plugin and paste it into your AI
   agent.

<details>
<summary>New to the terminal?</summary>

1. Open Terminal: press Cmd+Space, type `Terminal`, then press Return.
2. Install Homebrew from [brew.sh](https://brew.sh). Follow the install
   command shown on that page. On a managed Mac, installing Homebrew can
   need admin rights, so ask IT first.
3. Run the 2 turbofig commands above.
4. In Figma's file picker, press Cmd+Shift+G, then paste the manifest
   path. `turbofig setup` already copied it to your clipboard.

</details>

## Connect your agent

**The file bridge (default).** Click the copy-prompt button in the plugin
panel and paste the prompt into your agent. The agent then drives turbofig
by writing a JSON job to `~/.turbofig/inbox` and reading the result from
`~/.turbofig/outbox`. No curl, no MCP connection, no permission dialog. See
`skills/file-bridge.md`.

**Any agent, not just Claude.** [`docs/agents.md`](docs/agents.md) is an
agent-neutral onboarding prompt. Paste it into Cursor, Codex, Copilot,
Claude, or any other agent that can read and write local files or call an
MCP server.

**Claude Code (MCP):**

```bash
claude mcp add --transport http turbofig http://127.0.0.1:18846/mcp
```

**Any other MCP client that supports streamable HTTP:** point it at
`http://127.0.0.1:18846/mcp`. For a client with an allowlisted native MCP
config, merge this block (`docs/discoverability/mcp-config.json`):

```json
{
  "mcpServers": {
    "turbofig": {
      "type": "http",
      "url": "http://127.0.0.1:18846/mcp"
    }
  }
}
```

## What you can ask it

Any instruction that a Figma Plugin API script can carry out. For example:

- Tidy the auto layout spacing and padding across a whole page.
- Build 20 card variants from a list of titles and images.
- Audit every fill against the file's color variables and flag mismatches.
- Rename layers across a page to match a naming rule.
- Lay out a slide deck from a text outline.
- Screenshot a frame so the agent can check its own work before you look.

## The 4 tools

| Tool | What it does |
|---|---|
| `turbofig_execute` | Runs arbitrary Figma Plugin API JavaScript in the connected file and returns its result |
| `turbofig_get_selection` | Returns the current selection as a compact, shaped object |
| `turbofig_screenshot` | Exports a PNG of the file or a node, downscaled by default |
| `turbofig_status` | Returns connection state for the daemon and the connected plugin |

Every tool accepts an optional `fileKey` to target one of several open
files. Omit it to use the paired or sole connected file.

`turbofig_execute` example, matching the helper API in `helpers/tf-api.md`:

```js
await tf.loadFonts([{ family: "Inter", style: "Bold" }]);

const card = tf.frame({
  name: "Card",
  direction: "VERTICAL",
  gap: 12,
  padding: 24,
  fill: "#FFFFFF",
});

const heading = await tf.text({ text: "Card Title", size: 20, style: "Bold" });
tf.append(card, heading);
figma.currentPage.appendChild(card);
tf.commit("card");
return card.id;
```

A batch example, building many nodes from a data array in one
`turbofig_execute` call:

```js
const rows = [
  { label: "Alpha", color: "#0066FF" },
  { label: "Beta", color: "#00AA55" },
  { label: "Gamma", color: "#AA0066" },
];
await tf.loadFonts([{ family: "Inter", style: "Regular" }]);

const list = tf.frame({ name: "List", direction: "VERTICAL", gap: 8 });
for (const row of rows) {
  const chip = tf.frame({ direction: "HORIZONTAL", padding: 8, fill: row.color });
  const label = await tf.text({ text: row.label, size: 14, color: "#FFFFFF" });
  tf.append(chip, label);
  tf.append(list, chip);
}
figma.currentPage.appendChild(list);
tf.commit("list");
return list.id;
```

## How it works

- The daemon is one Rust process. It runs the MCP HTTP endpoint, the
  plugin WebSocket server, and the file bridge as three tasks sharing one
  state.
- The Figma plugin UI holds a persistent WebSocket to the daemon, with
  infinite-backoff reconnect, so a plugin reopen re-pairs with no
  handshake.
- MCP HTTP (`POST /mcp`, port 18846) is the native-client path: a request
  arrives over HTTP and the daemon forwards the call to the plugin.
- The file bridge (`~/.turbofig/inbox` and `outbox`) is the default path
  for a locked-down client: it writes a job file, the daemon's watcher
  picks it up, and it reads the result file back.
- The plugin WebSocket upgrade requires a per-install pairing token
  (`~/.turbofig/token`), so a malicious web page cannot open the socket
  even though it shares the plugin iframe's null Origin.

## Commands

| Command | Does |
|---|---|
| `turbofig setup` | Installs the pairing token and plugin files, writes and loads the launchd service, prints the 3 connect steps |
| `turbofig status` | Queries the running daemon's `/health` endpoint and prints a readable report |
| `turbofig uninstall [--purge]` | Unloads the launchd service and removes its plist. With `--purge`, also removes the turbofig home folder's own files (the token, the plugin files, the inbox, the outbox, the log), and removes the folder itself only if it is then empty |
| `turbofig serve` | Runs the daemon in the foreground. Same as no subcommand |

**Updating:**

```bash
brew upgrade turbofig
```

The daemon detects the upgrade, drains in-flight jobs, and restarts itself.
Reopen the plugin in Figma afterward so it picks up the refreshed plugin
files.

**Uninstalling:** run `turbofig uninstall` before `brew uninstall turbofig`,
so the launchd service is unloaded first.

## Configuration

All settings are environment variables. Each falls back to its default on
an absent, unparsable, or zero value.

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | `18846` | HTTP MCP port |
| `TURBOFIG_WS_PORT` | `18847` | Plugin WebSocket port |
| `TURBOFIG_REQUEST_TIMEOUT_MS` | `30000` | Wait for a plugin reply before returning a timeout result. Clamped to `600000` |
| `TURBOFIG_BRIDGE_DIR` | `~/.turbofig` | File-bridge home: the pairing token, the plugin files, and the inbox/outbox |

`turbofig setup` copies the env vars set at that time into the launchd
service. To change a setting for the background service, set it and run
`turbofig setup` again.

## Security

`turbofig_execute` runs arbitrary Figma Plugin API JavaScript by design.
Any client that can call this tool has full script access to the open
Figma file. Both ports bind to `127.0.0.1` only. The HTTP MCP port rejects
any request carrying an `Origin` header; the WebSocket port accepts only a
null or missing `Origin` and requires a pairing token on the upgrade. See
[SECURITY.md](SECURITY.md) for the full threat model and how to report a
vulnerability.

## Reliability and retries

- **launchd restarts the daemon if it exits.** The service runs with
  `KeepAlive`, so a crash is followed by a restart, not a dead daemon.
- **A job in flight when the plugin disconnects, or that times out, may
  still be running.** A retry of that job is not idempotent: the first
  attempt can still complete in Figma after you retry.
- **"Expired in the queue; the job did not run" is safe to retry.** This
  reply means the job's deadline passed before it started, so nothing ran.
- **Use a unique job id for every job.** Reusing an id while the first job
  with that id may still be running does not get you a result; see
  `skills/file-bridge.md`.

## Troubleshooting

- **The plugin panel shows disconnected.** Run `turbofig status` to check
  the daemon is up. If it is not, run `turbofig setup` again.
- **"Import plugin from manifest" is missing from the Plugins menu.** Use
  Figma Desktop, not the Figma web app. The menu item does not exist there.
- **A port is already in use.** Set `TURBOFIG_MCP_PORT` or
  `TURBOFIG_WS_PORT` to a free port and restart the daemon.
- **MCP is blocked on a managed machine.** Use the file bridge instead:
  click the copy-prompt button in the plugin panel, or read
  `skills/file-bridge.md` directly.
- **The panel or `turbofig status` warns of a version mismatch.** The
  daemon upgraded but the plugin in Figma has not reloaded. Reopen the
  turbofig plugin in Figma.

## Roadmap

- A screencast of a brief becoming a full Figma page.
- Wider platform support: Linux and Windows builds, `cargo install`, and a
  `curl | sh` installer.
- An auto-update nudge on daemon startup.
- A community-safe command vocabulary alongside the eval-first tool.
- Error codes in tool results, and an optional job audit log.
- Notarized binaries, for a company that needs them.

<!-- BENCHMARK RESULTS: add the measured section here after the first public run -->

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, and the PR
process, and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for the project's
code of conduct.

## License

MIT. See [LICENSE](LICENSE).
