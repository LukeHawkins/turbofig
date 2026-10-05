/**
 * Build script for the Turbofig plugin main thread.
 * Bundles src/code.ts as an IIFE and writes dist/code.js.
 * Run with: bun build-code.ts
 */

import { join } from "node:path";

const scriptDir = import.meta.dir;

// Read the plugin version from package.json to inject at build time, the
// same way build-ui.ts injects it into the UI bundle. The main thread
// reports this version in FILE_INFO (see protocol.ts's buildFileInfo), so
// the daemon and turbofig_status/`/health` can flag a stale plugin that
// needs reopening in Figma.
const pkgPath = join(scriptDir, "package.json");
const pkg = JSON.parse(await Bun.file(pkgPath).text()) as { version: string };
const pluginVersion = pkg.version;

const result = await Bun.build({
  entrypoints: [join(scriptDir, "src/code.ts")],
  outdir: join(scriptDir, "dist"),
  target: "browser",
  format: "iife",
  minify: false,
  define: {
    __PLUGIN_VERSION__: JSON.stringify(pluginVersion),
  },
});

if (!result.success) {
  for (const log of result.logs) {
    console.error(log);
  }
  process.exit(1);
}

console.log("Built dist/code.js");
