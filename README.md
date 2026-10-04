<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/wordmark-light.png">
    <img alt="turbofig wordmark" src="docs/wordmark-light.png" height="80">
  </picture>
</div>

Built to give AI full control of Figma with exceptional speed and token efficiency, powered by a Rust daemon. You spend less time waiting, and spend fewer tokens in the process.

Free and open source. Works on every Figma account — free, paid, and enterprise. Installed locally as a development plugin, so it works even on locked-down corporate Figma accounts.

Figma Desktop required. macOS. MIT licensed.

![Demo: a brief becomes a full Figma page in minutes](docs/demo.gif)

---

## Why

- Read and write access to Figma from AI needs a non-native solution. The official Figma MCP is read-only.
- The third-party solutions (`figma-console-mcp` and others) have real problems. They are slow, they are token-heavy, and the MCP handshake is frustrating to manage.
- On enterprise Figma accounts, native MCP servers are blocked. You end up with a workaround anyway.
- On enterprise machines, curl requests prompt for permission. The turbofig file-bridge avoids this. The AI only reads and writes files. It makes no network calls.
- The result: turbofig is always on and always paired. No handshake. No re-pairing. No curl prompts.

---

## Quickstart

### 1. Start the daemon

```bash
cargo build --release
./target/release/turbofig
```

To start the daemon at login, install the launchd service. This step is macOS only.

```bash
./install/install-macos.sh
```

The script builds the binary if needed, fills the template, and loads the service.

### 2. Open the plugin in Figma Desktop

Use Figma Desktop, not the web app. In Figma: **Plugins → Development → Import plugin from manifest**. Select `plugin/manifest.json`. Open a file and run the plugin. The panel shows the connection status and the file key.

### 3. Connect your AI

Click the copy button in the plugin panel. Paste the prompt into any AI with local file access. Claude Code is the example. That is it.

<details>
<summary>Fallback: MCP server</summary>

MCP is a fallback. It is heavier on tokens and it needs a network call. Add this to your MCP client config:

```json
{
  "mcpServers": {
    "turbofig": {
      "url": "http://127.0.0.1:18846/mcp"
    }
  }
}
```

</details>

<details>
<summary>Advanced: drive the file-bridge directly</summary>

Write a job file to the inbox:

```bash
cat > ~/.turbofig/inbox/job-001.json << 'EOF'
{
  "op": "execute",
  "js": "figma.createFrame(); 'done'"
}
EOF
```

Read the result from the outbox:

```bash
cat ~/.turbofig/outbox/job-001.json
```

</details>

---

## How it works

![How turbofig works](docs/how-it-works.png)

One Rust daemon process runs three servers: an HTTP MCP endpoint, a WebSocket server, and a file-bridge. The file-bridge is the primary path. The AI writes a job file and reads a result file, with no curl. The plugin holds a permanent WebSocket connection with automatic reconnect. One `execute` tool runs any Figma Plugin API JS, so one tool does the work of dozens.

---

## Tools

| Tool | What it does |
|---|---|
| `turbofig_execute` | Run arbitrary Figma Plugin API JS in the connected file |
| `turbofig_get_selection` | Return the current selection as a compact shaped object |
| `turbofig_screenshot` | Export a PNG of the file or a node (downscaled by default) |
| `turbofig_status` | Return connection state |

All capability flows through `execute`. The tool surface is locked at four.

Each tool accepts an optional `fileKey` to target one of several open files. Omit it to use the paired or sole connected file.

---

## Architecture

```
plugin/
  src/
    code.ts          # Figma main thread — dispatch, eval, plugin API
    ui/
      ui-logic.ts    # Pure UI state and message handlers
      main.ts        # Browser runtime
    manifest.json
daemon/
  src/
    main.rs          # MCP HTTP + WebSocket + file-bridge, one process
helpers/
  tf/                # tf.* craft namespace injected into every eval
skills/
  design.md          # Brief → plan → parallel builder subagents → QA → refine
```

Ports are a product contract, not a dev convention:

| Server | Default | Env var |
|---|---|---|
| HTTP MCP | `18846` | `TURBOFIG_MCP_PORT` |
| WebSocket | `18847` | `TURBOFIG_WS_PORT` |

Other env vars: `TURBOFIG_REQUEST_TIMEOUT_MS` (default `30000`), `TURBOFIG_BRIDGE_DIR` (default `~/.turbofig`).

---

## License

MIT. See LICENSE.
