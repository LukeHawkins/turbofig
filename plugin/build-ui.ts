/**
 * Build script for the Turbofig plugin UI.
 * Bundles src/ui/main.ts as an IIFE, injects it into src/ui/template.html,
 * and writes dist/ui.html. Run with: bun build-ui.ts
 */

import { mkdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const TOKEN = "__UI_BUNDLE__";
const scriptDir = import.meta.dir;

/**
 * Placeholder injected in place of a real pairing token. Must match the
 * literal `daemon/src/plugin_files.rs` replaces with the real token when it
 * writes the embedded plugin out to `~/.turbofig/figma-plugin/`.
 */
const TOKEN_PLACEHOLDER = "__TURBOFIG_PAIRING_TOKEN__";

// Ensure the output directory exists.
mkdirSync(join(scriptDir, "dist"), { recursive: true });

// Read the plugin version from package.json to inject at build time.
const pkgPath = join(scriptDir, "package.json");
const pkg = JSON.parse(await Bun.file(pkgPath).text()) as { version: string };
const pluginVersion = pkg.version;

/**
 * Dev convenience: a manually dev-installed plugin (loaded straight from
 * `dist/ui.html` in Figma Desktop) needs a real token to connect, since the
 * daemon's WS upgrade now requires one. Read it from `<home>/token`, where
 * `home` defaults to `~/.turbofig` and honours `TURBOFIG_HOME` /
 * `TURBOFIG_BRIDGE_DIR` the same way the daemon does. If the daemon has never
 * run, the file does not exist yet: fall back to the placeholder and warn.
 *
 * This keeps the build fully reproducible in CI and in a release build: a
 * fresh checkout and a fresh CI runner have no `~/.turbofig/token`, so they
 * always embed the placeholder, and no real token ever reaches `dist/` there
 * or enters git (`dist/` is gitignored) or the compiled daemon binary.
 */
function readLocalToken(): string {
  const home =
    process.env.TURBOFIG_HOME ?? process.env.TURBOFIG_BRIDGE_DIR ?? join(homedir(), ".turbofig");
  const tokenPath = join(home, "token");
  try {
    const contents = readFileSync(tokenPath, "utf8").trim();
    if (contents) return contents;
  } catch {
    // Fall through to the placeholder below.
  }
  console.warn(
    "turbofig: no pairing token found at " +
      tokenPath +
      " - start the daemon once (it creates ~/.turbofig/token), then rebuild",
  );
  return TOKEN_PLACEHOLDER;
}

const pairingToken = readLocalToken();

// Bundle the UI entry point as an IIFE for inline use in a <script> tag.
// __PLUGIN_VERSION__ and __TURBOFIG_PAIRING_TOKEN__ are replaced with literal
// strings at build time.
const result = await Bun.build({
  entrypoints: [join(scriptDir, "src/ui/main.ts")],
  target: "browser",
  format: "iife",
  minify: false,
  define: {
    __PLUGIN_VERSION__: JSON.stringify(pluginVersion),
    __TURBOFIG_PAIRING_TOKEN__: JSON.stringify(pairingToken),
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

// Inject the bundle.
// Use the function form of replace so '$' sequences in the content are
// never interpreted as replacement patterns.
const html = template.replace(TOKEN, () => bundleText);
const outPath = join(scriptDir, "dist/ui.html");
await Bun.write(outPath, html);
console.log(`Built dist/ui.html (${html.length} bytes)`);
