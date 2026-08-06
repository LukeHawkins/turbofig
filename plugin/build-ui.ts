/**
 * Build script for the Turbofig plugin UI.
 * Bundles src/ui/main.ts as an IIFE, injects it into src/ui/template.html,
 * and writes dist/ui.html. Run with: bun build-ui.ts
 */

import { mkdirSync } from "node:fs";
import { join } from "node:path";

const TOKEN = "__UI_BUNDLE__";
const scriptDir = import.meta.dir;

// Ensure the output directory exists.
mkdirSync(join(scriptDir, "dist"), { recursive: true });

// Read the plugin version from package.json to inject at build time.
const pkgPath = join(scriptDir, "package.json");
const pkg = JSON.parse(await Bun.file(pkgPath).text()) as { version: string };
const pluginVersion = pkg.version;

// Bundle the UI entry point as an IIFE for inline use in a <script> tag.
// __PLUGIN_VERSION__ is replaced with the literal version string at build time.
const result = await Bun.build({
  entrypoints: [join(scriptDir, "src/ui/main.ts")],
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

// Extract the single output.
const [output] = result.outputs;
if (!output) {
  console.error("Build produced no outputs.");
  process.exit(1);
}
const bundleText = await output.text();

// Read the HTML template.
const templatePath = join(scriptDir, "src/ui/template.html");
const template = await Bun.file(templatePath).text();

// Guard: the token must be present.
if (!template.includes(TOKEN)) {
  console.error(`Token "${TOKEN}" not found in ${templatePath}`);
  process.exit(1);
}

// Inject the bundle and write the output file.
const html = template.replace(TOKEN, bundleText);
const outPath = join(scriptDir, "dist/ui.html");
await Bun.write(outPath, html);
console.log(`Built dist/ui.html (${html.length} bytes)`);
