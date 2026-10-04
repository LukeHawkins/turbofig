# Turbofig plugin

The Figma-side half of Turbofig. Thin by design: the UI iframe holds the
WebSocket to the daemon, the main thread runs the Figma Plugin API. See
`../ARCHITECTURE.md` for the full picture and `../CLAUDE.md` for the project hub.

## Build

```bash
bun install
bun run build
```

This bundles `src/code.ts` to `dist/code.js` and `src/ui/` to `dist/ui.html`
(`build-ui.ts` injects the bundled UI script into `src/ui/template.html`).
Figma loads `dist/code.js` and `dist/ui.html` directly, per `manifest.json`.

**Rebuild after every edit under `src/`.** Figma reads only `dist/`; an edit
to a source file does nothing in the plugin until you rebuild.

## Watch

```bash
bun run watch
```

Rebuilds on every change under `src/`, debounced by 100ms.

## Import into Figma Desktop

Figma Desktop only; the web app cannot load a development plugin.

1. Build the plugin (above).
2. In Figma: **Plugins → Development → Import plugin from manifest…**
3. Select `plugin/manifest.json`.
4. Open a file and run the plugin from the same menu.

## Test and typecheck

Run from the repo root, not from here (the scripts cover `bench/` and
`scripts/` too):

```bash
bun test
bun run typecheck
bun run lint
```

## Three manifest choices a contributor will ask about

`manifest.json` makes three choices that look unusual for a plugin aiming at
the Community listing. None of them is accidental:

- **`id: "turbofig-dev"`.** This is a development-plugin id, not a published
  Community id. Figma assigns a real id only once a plugin is actually
  submitted. Until then, a development import needs a placeholder, and
  `-dev` keeps it from being mistaken for a real one.
- **`enablePrivatePluginApi: true`.** Turbofig reads `figma.fileKey` to show
  the file key in the panel and to target a specific open file from the
  daemon. `figma.fileKey` is empty without this flag. The flag is also why
  the plugin cannot go through a normal Community review unchanged: a
  Community listing requires removing it, which removes the fileKey feature.
- **`networkAccess.allowedDomains: ["*"]`.** The plugin connects to the
  daemon over `ws://127.0.0.1:<port>`, and the port is user-configurable
  (`SET_PORT`). Figma's manifest format has no wildcard-port syntax
  (`ws://127.0.0.1:*` is not accepted), so an allow-all domain list is the
  only way to support a changed port without a manifest edit. The daemon
  itself only ever binds localhost; this does not grant the plugin access to
  any remote host.

These three are a deliberate trade for a local-first, enterprise-friendly
tool, not an oversight. A Community submission would need to drop
`enablePrivatePluginApi` (losing the fileKey row) and narrow
`allowedDomains` to a fixed port (losing the configurable-port feature).
