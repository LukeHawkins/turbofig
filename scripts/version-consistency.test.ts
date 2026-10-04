/**
 * Guards the single-version contract: the workspace version in the root
 * Cargo.toml, plugin/package.json, and the root package.json must always
 * match. Run `bun scripts/bump-version.ts <x.y.z>` to change the version;
 * never hand-edit one of the three.
 */

import { describe, expect, test } from "bun:test";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");

function readCargoWorkspaceVersion(text: string): string {
  const match = text.match(/\[workspace\.package\][^[]*?\bversion\s*=\s*"([^"]*)"/);
  if (!match) {
    throw new Error("No [workspace.package] version field found in Cargo.toml");
  }
  return match[1] as string;
}

describe("version consistency", () => {
  test("Cargo.toml, package.json and plugin/package.json agree", async () => {
    const cargoToml = await Bun.file(join(ROOT, "Cargo.toml")).text();
    const rootPkg = JSON.parse(await Bun.file(join(ROOT, "package.json")).text());
    const pluginPkg = JSON.parse(await Bun.file(join(ROOT, "plugin/package.json")).text());

    const cargoVersion = readCargoWorkspaceVersion(cargoToml);

    expect(rootPkg.version).toBe(cargoVersion);
    expect(pluginPkg.version).toBe(cargoVersion);
  });

  test("the daemon crate inherits the workspace version", async () => {
    const daemonCargoToml = await Bun.file(join(ROOT, "daemon/Cargo.toml")).text();
    expect(daemonCargoToml).toMatch(/version\.workspace\s*=\s*true/);
  });
});
