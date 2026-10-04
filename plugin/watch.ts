/**
 * Watch script for the Turbofig plugin.
 * Watches src/ recursively and rebuilds on changes using `bun run build`.
 * Debounces rapid changes with a 100ms delay.
 * Run with: bun watch.ts
 */

import { spawn } from "node:child_process";
import { watch } from "node:fs";
import { join } from "node:path";

const scriptDir = import.meta.dir;
const srcDir = join(scriptDir, "src");
const DEBOUNCE_MS = 100;

let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let building = false;
let pendingBuild = false;

function runBuild(): void {
  if (building) {
    pendingBuild = true;
    return;
  }

  building = true;
  console.log("[watch] Building...");

  const proc = spawn("bun", ["run", "build"], {
    cwd: scriptDir,
    stdio: "inherit",
  });

  proc.on("close", (code) => {
    building = false;
    if (code === 0) {
      console.log("[watch] Build complete.");
    } else {
      console.error(`[watch] Build failed (exit code ${code}).`);
    }

    if (pendingBuild) {
      pendingBuild = false;
      runBuild();
    }
  });
}

function scheduleBuild(): void {
  if (debounceTimer !== null) {
    clearTimeout(debounceTimer);
  }
  debounceTimer = setTimeout(() => {
    debounceTimer = null;
    runBuild();
  }, DEBOUNCE_MS);
}

// Run an initial build on start.
runBuild();

// Watch src/ recursively. Skip paths that contain dist/.
watch(srcDir, { recursive: true }, (_event, filename) => {
  if (filename?.includes("dist/")) return;
  scheduleBuild();
});

console.log(`[watch] Watching ${srcDir}`);
