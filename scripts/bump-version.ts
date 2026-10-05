/**
 * Version bump script.
 * Sets one semver across the repo: the root Cargo.toml workspace version,
 * plugin/package.json, and the root package.json. Refreshes Cargo.lock and
 * moves the CHANGELOG Unreleased entries under a new dated release heading.
 * Run with: bun scripts/bump-version.ts <x.y.z>
 *
 * Safe to run twice with the same target version: every step checks the
 * current value first and skips the write when it already matches.
 */

import { execFileSync } from "node:child_process";
import { join } from "node:path";

const SEMVER_RE = /^\d+\.\d+\.\d+$/;

const ROOT = join(import.meta.dir, "..");
const ROOT_CARGO_TOML = join(ROOT, "Cargo.toml");
const ROOT_PACKAGE_JSON = join(ROOT, "package.json");
const PLUGIN_PACKAGE_JSON = join(ROOT, "plugin/package.json");
const CHANGELOG = join(ROOT, "CHANGELOG.md");

/**
 * Parse and validate the target version from argv.
 */
export function parseTargetVersion(argv: string[]): string {
  const version = argv[0];
  if (!version || !SEMVER_RE.test(version)) {
    throw new Error(`Usage: bun scripts/bump-version.ts <x.y.z> (got: ${version ?? "<none>"})`);
  }
  return version;
}

/**
 * Set `version = "x.y.z"` under `[workspace.package]` in a Cargo.toml string.
 * Returns the updated text and whether a change was made.
 */
export function bumpWorkspaceCargoToml(
  text: string,
  version: string,
): { text: string; changed: boolean } {
  const re = /(\[workspace\.package\][^[]*?\bversion\s*=\s*")([^"]*)(")/;
  const match = text.match(re);
  if (!match) {
    throw new Error("No [workspace.package] version field found in Cargo.toml");
  }
  if (match[2] === version) {
    return { text, changed: false };
  }
  return { text: text.replace(re, `$1${version}$3`), changed: true };
}

/**
 * Set the top-level "version" field in a package.json string.
 * Preserves formatting by doing a targeted string replace, not a
 * parse-and-restringify round trip.
 */
export function bumpPackageJson(text: string, version: string): { text: string; changed: boolean } {
  const re = /("version"\s*:\s*")([^"]*)(")/;
  const match = text.match(re);
  if (!match) {
    throw new Error('No top-level "version" field found in package.json');
  }
  if (match[2] === version) {
    return { text, changed: false };
  }
  return { text: text.replace(re, `$1${version}$3`), changed: true };
}

/**
 * Move the Unreleased entries under a new dated release heading, and leave
 * an empty Unreleased section above it. Idempotent: if a heading for this
 * version already exists directly below Unreleased, do nothing.
 */
export function bumpChangelog(
  text: string,
  version: string,
  date: string,
): { text: string; changed: boolean } {
  const unreleasedHeadingRe = /^## \[Unreleased\].*$/m;
  const headingMatch = text.match(unreleasedHeadingRe);
  if (!headingMatch || headingMatch.index === undefined) {
    throw new Error("No `## [Unreleased]` heading found in CHANGELOG.md");
  }

  const afterHeadingStart = headingMatch.index + headingMatch[0].length;
  // Find the next top-level release heading (## [...]) after Unreleased, if any.
  const rest = text.slice(afterHeadingStart);
  const nextHeadingMatch = rest.match(/^## \[/m);
  const body = nextHeadingMatch ? rest.slice(0, nextHeadingMatch.index) : rest;
  const tail = nextHeadingMatch ? rest.slice(nextHeadingMatch.index) : "";

  const trimmedBody = body.replace(/^\n+/, "").replace(/\n+$/, "");
  const newReleaseHeading = `## [${version}] - ${date}`;

  if (tail.startsWith(newReleaseHeading)) {
    // Already bumped to this version; nothing left to move.
    return { text, changed: false };
  }

  if (trimmedBody.length === 0) {
    // Unreleased is already empty: nothing to move.
    return { text, changed: false };
  }

  const newText =
    text.slice(0, afterHeadingStart) +
    "\n\n" +
    newReleaseHeading +
    "\n\n" +
    trimmedBody +
    "\n\n" +
    tail;
  return { text: newText, changed: true };
}

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

/**
 * Refresh Cargo.lock for the new workspace version.
 * Throws when `cargo update` fails, so a stale lockfile never ships silently.
 * `run` is injectable for tests; it defaults to the real `execFileSync`.
 */
export async function refreshCargoLock(run: typeof execFileSync = execFileSync): Promise<void> {
  try {
    run("cargo", ["update", "-p", "turbofig", "--offline"], {
      cwd: ROOT,
      stdio: "pipe",
    });
  } catch (err) {
    const stderr =
      err && typeof err === "object" && "stderr" in err
        ? String((err as { stderr: Buffer }).stderr)
        : String(err);
    throw new Error(`cargo update failed, Cargo.lock was not refreshed: ${stderr}`);
  }
}

async function main() {
  const version = parseTargetVersion(process.argv.slice(2));
  const changed: string[] = [];

  const cargoTomlText = await Bun.file(ROOT_CARGO_TOML).text();
  const cargoTomlResult = bumpWorkspaceCargoToml(cargoTomlText, version);
  if (cargoTomlResult.changed) {
    await Bun.write(ROOT_CARGO_TOML, cargoTomlResult.text);
    changed.push("Cargo.toml");
  }

  const rootPkgText = await Bun.file(ROOT_PACKAGE_JSON).text();
  const rootPkgResult = bumpPackageJson(rootPkgText, version);
  if (rootPkgResult.changed) {
    await Bun.write(ROOT_PACKAGE_JSON, rootPkgResult.text);
    changed.push("package.json");
  }

  const pluginPkgText = await Bun.file(PLUGIN_PACKAGE_JSON).text();
  const pluginPkgResult = bumpPackageJson(pluginPkgText, version);
  if (pluginPkgResult.changed) {
    await Bun.write(PLUGIN_PACKAGE_JSON, pluginPkgResult.text);
    changed.push("plugin/package.json");
  }

  const changelogText = await Bun.file(CHANGELOG).text();
  const changelogResult = bumpChangelog(changelogText, version, today());
  if (changelogResult.changed) {
    await Bun.write(CHANGELOG, changelogResult.text);
    changed.push("CHANGELOG.md");
  }

  if (cargoTomlResult.changed) {
    await refreshCargoLock();
    changed.push("Cargo.lock");
  }

  if (changed.length === 0) {
    console.log(`Already at version ${version}. Nothing to change.`);
  } else {
    console.log(`Bumped to version ${version}:`);
    for (const path of changed) {
      console.log(`  ${path}`);
    }
  }
}

if (import.meta.main) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.message : String(err));
    process.exit(1);
  });
}
