/**
 * Guards the plugin manifest.
 *
 * `figma.fileKey` is a private-plugin API. It returns undefined unless the
 * manifest sets `enablePrivatePluginApi: true`. Without it, the panel never
 * receives a fileKey, so the fileKey row and both copy buttons stay hidden.
 * This test locks the flag on.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const manifest = JSON.parse(
  readFileSync(join(import.meta.dir, "../manifest.json"), "utf8"),
) as Record<string, unknown>;

describe("plugin manifest", () => {
  test("enables the private plugin API so figma.fileKey is populated", () => {
    expect(manifest.enablePrivatePluginApi).toBe(true);
  });

  test("uses dynamic-page document access", () => {
    expect(manifest.documentAccess).toBe("dynamic-page");
  });
});
