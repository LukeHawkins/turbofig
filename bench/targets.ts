/**
 * Target registry.
 *
 * Only one Figma plugin runs per file at a time, so the harness benchmarks
 * exactly one target per invocation (--target). A separate `compare` command
 * merges reports written by separate target runs.
 */

export type TargetId = "turbofig-mcp" | "turbofig-bridge" | "console-mcp";

export const TARGET_IDS: TargetId[] = ["turbofig-mcp", "turbofig-bridge", "console-mcp"];

export function isTargetId(value: string): value is TargetId {
  return (TARGET_IDS as string[]).includes(value);
}

/** Default MCP HTTP endpoint per target. Override with --mcp-url. */
export function defaultMcpUrl(target: TargetId): string {
  switch (target) {
    case "turbofig-mcp":
      return "http://127.0.0.1:18846/mcp";
    case "console-mcp":
      return "http://127.0.0.1:3846/mcp";
    case "turbofig-bridge":
      throw new Error("turbofig-bridge has no MCP URL; it uses --bridge-dir");
  }
}

export function targetDescription(target: TargetId): string {
  switch (target) {
    case "turbofig-mcp":
      return "turbofig over MCP HTTP (tool turbofig_execute)";
    case "turbofig-bridge":
      return "turbofig over its file bridge (~/.turbofig/inbox -> outbox)";
    case "console-mcp":
      return "figma-console-mcp over MCP HTTP (tool figma_execute)";
  }
}
