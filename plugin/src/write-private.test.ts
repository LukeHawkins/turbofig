import { describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { writeOwnerOnly } from "../write-private";

describe("writeOwnerOnly", () => {
  test("creates a new file with mode 0600", () => {
    const path = join(mkdtempSync(join(tmpdir(), "tf-wp-")), "ui.html");
    writeOwnerOnly(path, "a");
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(readFileSync(path, "utf8")).toBe("a");
  });

  test("tightens an existing world-readable file to 0600", () => {
    const path = join(mkdtempSync(join(tmpdir(), "tf-wp-")), "ui.html");
    writeFileSync(path, "old");
    chmodSync(path, 0o644);
    writeOwnerOnly(path, "new");
    expect(statSync(path).mode & 0o777).toBe(0o600);
    expect(readFileSync(path, "utf8")).toBe("new");
  });
});
