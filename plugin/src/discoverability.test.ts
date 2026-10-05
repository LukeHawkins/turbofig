/**
 * Tests for the discoverability artifacts in docs/discoverability/.
 * These tests are file-based: they read the artifact files and assert their contents.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const DIST = join(import.meta.dir, "../../docs/discoverability");

describe("CLAUDE-snippet.md", () => {
  const content = readFileSync(join(DIST, "CLAUDE-snippet.md"), "utf-8");

  test("contains the MCP port 18846", () => {
    expect(content).toContain("18846");
  });

  test("names the turbofig_execute tool", () => {
    expect(content).toContain("turbofig_execute");
  });

  test("mentions the file-bridge path (inbox)", () => {
    const hasBridge = content.includes("file-bridge") || content.includes("inbox");
    expect(hasBridge).toBe(true);
  });

  test("explains the fileKey targeting model", () => {
    expect(content).toContain("fileKey");
  });
});

describe("mcp-config.json", () => {
  const raw = readFileSync(join(DIST, "mcp-config.json"), "utf-8");
  const config = JSON.parse(raw) as Record<string, unknown>;

  test("parses as valid JSON", () => {
    expect(config).toBeDefined();
  });

  test("has a mcpServers.turbofig entry", () => {
    const servers = config["mcpServers"] as Record<string, unknown>;
    expect(servers).toBeDefined();
    expect(servers["turbofig"]).toBeDefined();
  });

  test("the turbofig entry runs the turbofig binary", () => {
    const servers = config["mcpServers"] as Record<string, unknown>;
    const entry = servers["turbofig"] as Record<string, unknown>;
    const command = entry["command"] as string;
    expect(command).toContain("turbofig");
  });

  test("the turbofig entry args pass the mcp subcommand", () => {
    const servers = config["mcpServers"] as Record<string, unknown>;
    const entry = servers["turbofig"] as Record<string, unknown>;
    const args = entry["args"] as string[];
    expect(args).toContain("mcp");
  });
});
