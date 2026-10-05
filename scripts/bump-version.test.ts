/**
 * Tests for the version bump helpers.
 * Every test operates on temp copies of the real files (or on in-memory
 * strings), never on the real Cargo.toml, package.json, or CHANGELOG.md.
 */

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  bumpChangelog,
  bumpPackageJson,
  bumpWorkspaceCargoToml,
  parseTargetVersion,
  refreshCargoLock,
} from "./bump-version.ts";

let tmpDir: string;

beforeEach(() => {
  tmpDir = mkdtempSync(join(tmpdir(), "bump-version-test-"));
});

afterEach(() => {
  rmSync(tmpDir, { recursive: true, force: true });
});

describe("parseTargetVersion", () => {
  test("accepts a valid semver", () => {
    expect(parseTargetVersion(["1.2.3"])).toBe("1.2.3");
  });

  test("rejects a missing argument", () => {
    expect(() => parseTargetVersion([])).toThrow();
  });

  test("rejects a non-semver string", () => {
    expect(() => parseTargetVersion(["1.2"])).toThrow();
    expect(() => parseTargetVersion(["v1.2.3"])).toThrow();
    expect(() => parseTargetVersion(["1.2.3-rc1"])).toThrow();
  });
});

describe("bumpWorkspaceCargoToml", () => {
  test("sets the workspace.package version on a temp copy", async () => {
    const src = join(tmpDir, "Cargo.toml");
    const text = [
      "[workspace]",
      'members = ["daemon"]',
      "",
      "[workspace.package]",
      'version = "0.1.0"',
      'rust-version = "1.94"',
      "",
    ].join("\n");
    await Bun.write(src, text);
    const original = await Bun.file(src).text();

    const result = bumpWorkspaceCargoToml(original, "0.2.0");
    expect(result.changed).toBe(true);
    expect(result.text).toContain('version = "0.2.0"');
    expect(result.text).toContain('rust-version = "1.94"');
  });

  test("is a no-op when the version already matches", () => {
    const text = '[workspace.package]\nversion = "0.2.0"\n';
    const result = bumpWorkspaceCargoToml(text, "0.2.0");
    expect(result.changed).toBe(false);
    expect(result.text).toBe(text);
  });

  test("throws when no workspace.package version field exists", () => {
    expect(() => bumpWorkspaceCargoToml("[workspace]\n", "0.2.0")).toThrow();
  });
});

describe("bumpPackageJson", () => {
  test("sets the top-level version on a temp copy", async () => {
    const src = join(tmpDir, "package.json");
    const text = '{\n  "name": "turbofig",\n  "version": "0.1.0"\n}\n';
    await Bun.write(src, text);
    const original = await Bun.file(src).text();

    const result = bumpPackageJson(original, "0.2.0");
    expect(result.changed).toBe(true);
    expect(JSON.parse(result.text).version).toBe("0.2.0");
    expect(JSON.parse(result.text).name).toBe("turbofig");
  });

  test("is a no-op when the version already matches", () => {
    const text = '{"version": "0.2.0"}';
    const result = bumpPackageJson(text, "0.2.0");
    expect(result.changed).toBe(false);
  });
});

describe("bumpChangelog", () => {
  test("moves Unreleased entries under a new dated heading on a temp copy", async () => {
    const src = join(tmpDir, "CHANGELOG.md");
    const text = [
      "# Changelog",
      "",
      "## [Unreleased] - 0.1.0",
      "",
      "### Added",
      "",
      "- Something new.",
      "",
    ].join("\n");
    await Bun.write(src, text);
    const original = await Bun.file(src).text();

    const result = bumpChangelog(original, "0.2.0", "2026-10-04");
    expect(result.changed).toBe(true);
    expect(result.text).toContain("## [Unreleased]");
    expect(result.text).toContain("## [0.2.0] - 2026-10-04");
    // The entry moved below the new release heading, not above it.
    const unreleasedIdx = result.text.indexOf("## [Unreleased]");
    const releaseIdx = result.text.indexOf("## [0.2.0]");
    const entryIdx = result.text.indexOf("- Something new.");
    expect(unreleasedIdx).toBeLessThan(releaseIdx);
    expect(releaseIdx).toBeLessThan(entryIdx);
    // Unreleased itself is left empty (no entry text between the two headings).
    const betweenHeadings = result.text.slice(unreleasedIdx + "## [Unreleased]".length, releaseIdx);
    expect(betweenHeadings).not.toContain("Something new");
  });

  test("is a no-op when Unreleased is already empty", () => {
    const text = [
      "# Changelog",
      "",
      "## [Unreleased]",
      "",
      "## [0.1.0] - 2026-01-01",
      "",
      "### Added",
      "",
      "- Old entry.",
      "",
    ].join("\n");
    const result = bumpChangelog(text, "0.2.0", "2026-10-04");
    expect(result.changed).toBe(false);
    expect(result.text).toBe(text);
  });

  test("is idempotent: running twice with the same version changes nothing the second time", () => {
    const text = ["# Changelog", "", "## [Unreleased] - 0.1.0", "", "- Something new.", ""].join(
      "\n",
    );
    const first = bumpChangelog(text, "0.2.0", "2026-10-04");
    expect(first.changed).toBe(true);
    const second = bumpChangelog(first.text, "0.2.0", "2026-10-04");
    expect(second.changed).toBe(false);
    expect(second.text).toBe(first.text);
  });

  test("throws when no Unreleased heading exists", () => {
    expect(() => bumpChangelog("# Changelog\n", "0.2.0", "2026-10-04")).toThrow();
  });
});

describe("refreshCargoLock", () => {
  test("throws a clear error when cargo update fails", async () => {
    const failingRun = () => {
      const err = new Error("exit code 1") as Error & { stderr: Buffer };
      err.stderr = Buffer.from("error: could not reach registry");
      throw err;
    };
    await expect(
      refreshCargoLock(failingRun as unknown as typeof import("node:child_process").execFileSync),
    ).rejects.toThrow(/cargo update failed/);
  });

  test("resolves without throwing when cargo update succeeds", async () => {
    const okRun = () => Buffer.from("");
    await expect(
      refreshCargoLock(okRun as unknown as typeof import("node:child_process").execFileSync),
    ).resolves.toBeUndefined();
  });
});
