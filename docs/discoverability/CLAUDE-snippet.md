# Turbofig: Figma design agent (local daemon)

Turbofig may be running on this machine. It exposes an HTTP MCP server on port 18846.

**First step:** GET `http://127.0.0.1:18846/` to receive a short help payload that describes every available operation.

## Four tools

- `turbofig_execute`: run Figma Plugin API JavaScript in the open file
- `turbofig_get_selection`: read the current selection
- `turbofig_screenshot`: capture the canvas or a node
- `turbofig_status`: return connection and taste-profile state

Each tool accepts an optional `fileKey` parameter to target a specific open file. Omit `fileKey` to use the paired file or the sole connected file.

## File-bridge (no MCP required)

Write a JSON job to `~/.turbofig/inbox/`. Read the result from `~/.turbofig/outbox/`. Operations mirror the four tools: `execute`, `get_selection`, `screenshot`, `status`.

## Targeting and taste

- **fileKey targeting:** one daemon serves all open Figma files. Pass a `fileKey` to target one file explicitly.
- **Taste on by default:** each file has an active taste profile (default: `impeccable`) injected as `tf.taste`. Generated designs must obey it. Call `turbofig_status` to read the active `profileId`.
