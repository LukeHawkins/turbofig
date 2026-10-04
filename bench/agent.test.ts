/**
 * Unit tests for bench/agent.ts pure functions.
 * Never spawns `claude`. Run with: bun test bench/agent.test.ts
 */

import { describe, expect, it } from "bun:test";
import { AGENT_TARGETS, AGENT_TASKS, buildArgs, formatCommand, parseResult } from "./agent.js";

describe("buildArgs", () => {
  const cfg = AGENT_TARGETS[0];
  const task = AGENT_TASKS[0];

  it("includes --strict-mcp-config so only the target's own tools load", () => {
    expect(buildArgs(cfg, task)).toContain("--strict-mcp-config");
  });

  it("includes --bare so no project CLAUDE.md leaks into context", () => {
    expect(buildArgs(cfg, task)).toContain("--bare");
  });

  it("scopes --allowedTools to the target's own MCP server", () => {
    const argv = buildArgs(cfg, task);
    const idx = argv.indexOf("--allowedTools");
    expect(argv[idx + 1]).toBe(`mcp__${cfg.serverName}__*`);
  });

  it("passes --output-format json", () => {
    const argv = buildArgs(cfg, task);
    const idx = argv.indexOf("--output-format");
    expect(argv[idx + 1]).toBe("json");
  });

  it("passes the task prompt to -p", () => {
    const argv = buildArgs(cfg, task);
    expect(argv[0]).toBe("-p");
    expect(argv[1]).toBe(task.prompt);
  });
});

describe("formatCommand", () => {
  it("quotes an argument containing whitespace", () => {
    const cmd = formatCommand(["-p", "reply with OK"]);
    expect(cmd).toBe('claude -p "reply with OK"');
  });

  it("leaves a plain flag unquoted", () => {
    const cmd = formatCommand(["--bare"]);
    expect(cmd).toBe("claude --bare");
  });
});

describe("parseResult", () => {
  it("reads token usage, turns, and duration from a successful result", () => {
    const stdout = JSON.stringify({
      is_error: false,
      duration_ms: 4200,
      num_turns: 2,
      total_cost_usd: 0.01,
      usage: {
        input_tokens: 1500,
        output_tokens: 80,
        cache_creation_input_tokens: 900,
        cache_read_input_tokens: 0,
      },
    });
    const record = parseResult("turbofig-mcp", "trivial", 0, stdout);
    expect(record.ok).toBe(true);
    expect(record.inputTokens).toBe(1500);
    expect(record.outputTokens).toBe(80);
    expect(record.cacheCreationTokens).toBe(900);
    expect(record.numTurns).toBe(2);
    expect(record.durationMs).toBe(4200);
    expect(record.error).toBeNull();
  });

  it("marks ok false and carries the result text as the error when is_error is true", () => {
    const stdout = JSON.stringify({ is_error: true, result: "tool call failed" });
    const record = parseResult("console-mcp", "create-frame", 1, stdout);
    expect(record.ok).toBe(false);
    expect(record.error).toBe("tool call failed");
  });

  it("marks ok false with a parse-error message when stdout is not JSON", () => {
    const record = parseResult("turbofig-mcp", "trivial", 0, "not json at all");
    expect(record.ok).toBe(false);
    expect(record.error).toContain("failed to parse claude JSON output");
  });

  it("returns null for every numeric field missing from a minimal result", () => {
    const record = parseResult("turbofig-mcp", "trivial", 0, JSON.stringify({}));
    expect(record.ok).toBe(true);
    expect(record.inputTokens).toBeNull();
    expect(record.durationMs).toBeNull();
    expect(record.numTurns).toBeNull();
  });
});
