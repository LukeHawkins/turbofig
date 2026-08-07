<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/wordmark-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/wordmark-light.png">
  <img alt="turbofig wordmark" src="docs/wordmark-light.png" height="40">
</picture>

**Bridge any AI to Figma.**

> **Status:** In development — not yet released.

---

Drive Figma from any AI agent. Write a JSON job file. Read the result. No re-pairing, no session management, no extra process to keep alive.

![Demo: a brief becomes a full Figma page in minutes](docs/demo.gif)

---

## Why

Existing MCP-based Figma tools have two problems.

First, they are session-coupled. When your Claude session ends or crashes, the connection dies. You pair again. Mid-job crashes lose work.

Second, they are token-heavy. Full node trees, unscaled screenshots, no batching. Long jobs cost a lot.

Turbofig fixes both. A Rust daemon runs at login, always on, independent of any AI session. The plugin reconnects automatically. An `execute` tool runs arbitrary Figma Plugin API JS, so one tool does the work of dozens. Shaped returns, downscaled screenshots, and a helper library cut token use to a fraction of naive approaches.

---

## How it works

```
AI  ──────────────────────────────────────────────────►  Rust daemon  ──WS──►  Figma plugin  ──►  Figma
    file-bridge  ~/.turbofig/inbox → outbox             :18846 (HTTP MCP)       (eval)
    MCP HTTP     POST :18846/mcp                        :18847 (WebSocket)
```

One daemon process runs three servers:

- **HTTP MCP endpoint** (`POST http://127.0.0.1:18846/mcp`). Any MCP-capable client connects here.
- **WebSocket server** (`:18847`). The Figma plugin holds a permanent connection with infinite-backoff reconnect.
- **File-bridge**. Write a JSON job to `~/.turbofig/inbox/`. Read the result from `outbox/`. No curl, no MCP client required. This is the primary path — fastest, zero permission prompts, token-light.

A plain `GET http://127.0.0.1:18846` returns a short help payload. An AI told only the port can bootstrap without reading this repo.

---

## Four tools

| Tool | What it does |
|---|---|
| `turbofig_execute` | Run arbitrary Figma Plugin API JS in the connected file |
| `turbofig_get_selection` | Return the current selection as a compact shaped object |
| `turbofig_screenshot` | Export a PNG of the file or a node (downscaled by default) |
| `turbofig_status` | Return connection state and the active taste profile |

All capability flows through `execute`. The tool surface is locked at four.

Each tool accepts an optional `fileKey` to target one of several open files. Omit it to use the paired or sole connected file.

---

## Quickstart

### 1. Start the daemon

```bash
cargo build --release
./target/release/turbofig-daemon
```

Or install the launchd service so the daemon starts at login:

```bash
cp launchd/eu.lukehawkins.turbofig.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/eu.lukehawkins.turbofig.plist
```

### 2. Install the plugin in Figma

In Figma: **Plugins → Development → Import plugin from manifest**.

Select `plugin/manifest.json`. The plugin requires `enablePrivatePluginApi: true` to read the file key — this means a dev install for now.

Open a file and run the plugin. The panel shows the connection status and the file key.

### 3. Point an AI at it

**Option A — file-bridge (recommended).** Write a job file:

```bash
cat > ~/.turbofig/inbox/job-001.json << 'EOF'
{
  "op": "execute",
  "js": "figma.createFrame(); 'done'"
}
EOF
```

Read the result:

```bash
cat ~/.turbofig/outbox/job-001.json
```

**Option B — MCP.** Add the MCP server to your AI client config:

```json
{
  "mcpServers": {
    "turbofig": {
      "url": "http://127.0.0.1:18846/mcp"
    }
  }
}
```

**Option C — copy the connect prompt.** Click "Copy prompt" in the plugin panel. Paste it into a Claude Code session. The prompt contains the MCP port, the daemon help URL, and this file's key.

---

## Taste profiles

Turbofig injects a taste profile into every `execute` call. The profile sets spacing, type, palette, and anti-slop rules for the active file. The AI reads `tf.taste` and applies them.

Three built-in profiles, swappable per file from the plugin panel:

| Profile | Character |
|---|---|
| `impeccable` (default) | Neutral, precise, anti-slop floor |
| `editorial` | Asymmetric, dense, expressive type |
| `minimal` | Genuinely restrained, anti-slop floor preserved |

The choice persists in the Figma document. One daemon serves many files; each file runs its own profile.

Add a custom profile: drop a `.js` file in `~/.turbofig/profiles/`. The daemon loads it by stem name at startup.

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
  profiles/          # impeccable.js, editorial.js, minimal.js
  design.md          # Brief → plan → parallel builder subagents → QA → refine
```

**Ports** are a product contract, not a dev convention:

| Server | Default | Env var |
|---|---|---|
| HTTP MCP | `18846` | `TURBOFIG_MCP_PORT` |
| WebSocket | `18847` | `TURBOFIG_WS_PORT` |

Other env vars: `TURBOFIG_REQUEST_TIMEOUT_MS` (default `30000`), `TURBOFIG_BRIDGE_DIR` (default `~/.turbofig`), `TURBOFIG_PROFILES_DIR` (default `~/.turbofig/profiles`).

---

## Links

- [lukehawkins.eu](https://lukehawkins.eu)
- [github.com/lukehawkins/turbofig](https://github.com/lukehawkins/turbofig)
