/**
 * Agent-layer benchmark.
 *
 * The transport layer (harness.ts) measures wire bytes. It leaves out the
 * per-session cost of loading a target's tool schemas into a real agent
 * context. This script measures that cost directly: it runs a headless
 * Claude Code session once per (target, task, iteration) and records the
 * token usage, turn count, duration, and success that Claude Code itself
 * reports.
 *
 * Each target gets only what ships with it: `--mcp-config <target's file>
 * --strict-mcp-config` so no other MCP server's tools load, `--bare` so no
 * project CLAUDE.md or skill leaks into context, and `--allowedTools
 * "mcp__<server>__*"` so the session can reach only that target's own tools
 * (no Bash, no Edit, nothing that could touch the filesystem). The "trivial"
 * task ("reply OK", no tool call) isolates the fixed tool-schema cost as a
 * pure input-token difference between targets, because no other work runs.
 *
 * This script never calls `claude -p` with a real prompt on its own.
 * `bun bench/agent.ts --dry-run` prints the exact commands it would run
 * without spawning them. A real run spends Claude Code usage, so the owner
 * runs it deliberately: `bun bench/agent.ts --runs 5 --out agent-report.json`.
 *
 * Every flag here was checked against `claude --help` before use: -p,
 * --mcp-config, --strict-mcp-config, --bare, --permission-mode,
 * --allowedTools, --output-format are all real, documented flags.
 */

import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

export interface AgentTask {
  id: string;
  prompt: string;
}

/** The trivial task carries no tool call, so its input-token count shows the
 * fixed per-session cost of a target's tool schemas with nothing else in
 * the way. The second task forces exactly one tool call on both targets. */
export const AGENT_TASKS: AgentTask[] = [
  {
    id: "trivial",
    prompt: "Reply with exactly the word OK. Do not call any tool.",
  },
  {
    id: "create-frame",
    prompt:
      'Using the available Figma tool, create one frame named "Agent Bench", 400x300, white fill, on the current page. Reply with the node id you created.',
  },
];

export interface TargetAgentConfig {
  target: "turbofig-mcp" | "console-mcp";
  mcpConfigFile: string;
  serverName: string;
}

export const AGENT_TARGETS: TargetAgentConfig[] = [
  {
    target: "turbofig-mcp",
    mcpConfigFile: join(__dirname, "mcp-config.turbofig.json"),
    serverName: "turbofig",
  },
  {
    target: "console-mcp",
    mcpConfigFile: join(__dirname, "mcp-config.console-mcp.json"),
    serverName: "console-mcp",
  },
];

/**
 * The fields this script reads from `claude -p ... --output-format json`
 * stdout. The CLI emits more fields than this; everything else is ignored.
 */
export interface ClaudeJsonResult {
  type?: string;
  subtype?: string;
  is_error?: boolean;
  duration_ms?: number;
  duration_api_ms?: number;
  num_turns?: number;
  result?: string;
  session_id?: string;
  total_cost_usd?: number;
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    cache_creation_input_tokens?: number;
    cache_read_input_tokens?: number;
  };
}

/** One (target, task, iteration) measurement. */
export interface AgentRunRecord {
  target: string;
  task: string;
  iteration: number;
  ok: boolean;
  durationMs: number | null;
  numTurns: number | null;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheCreationTokens: number | null;
  cacheReadTokens: number | null;
  totalCostUsd: number | null;
  error: string | null;
}

/**
 * Build the exact argv for one (target, task) invocation. Pure, so --dry-run
 * and the real runner share one source of truth for the command line.
 */
export function buildArgs(cfg: TargetAgentConfig, task: AgentTask): string[] {
  return [
    "-p",
    task.prompt,
    "--mcp-config",
    cfg.mcpConfigFile,
    "--strict-mcp-config",
    "--bare",
    "--permission-mode",
    "bypassPermissions",
    "--allowedTools",
    `mcp__${cfg.serverName}__*`,
    "--output-format",
    "json",
  ];
}

/** Parse one `claude -p ... --output-format json` stdout string into a record. */
export function parseResult(
  target: string,
  task: string,
  iteration: number,
  stdout: string,
): AgentRunRecord {
  let parsed: ClaudeJsonResult;
  try {
    parsed = JSON.parse(stdout) as ClaudeJsonResult;
  } catch (err) {
    return {
      target,
      task,
      iteration,
      ok: false,
      durationMs: null,
      numTurns: null,
      inputTokens: null,
      outputTokens: null,
      cacheCreationTokens: null,
      cacheReadTokens: null,
      totalCostUsd: null,
      error: `failed to parse claude JSON output: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
  return {
    target,
    task,
    iteration,
    ok: parsed.is_error !== true,
    durationMs: parsed.duration_ms ?? null,
    numTurns: parsed.num_turns ?? null,
    inputTokens: parsed.usage?.input_tokens ?? null,
    outputTokens: parsed.usage?.output_tokens ?? null,
    cacheCreationTokens: parsed.usage?.cache_creation_input_tokens ?? null,
    cacheReadTokens: parsed.usage?.cache_read_input_tokens ?? null,
    totalCostUsd: parsed.total_cost_usd ?? null,
    error: parsed.is_error ? (parsed.result ?? "unknown error") : null,
  };
}

/** Render one invocation as a copy-pasteable shell command, for --dry-run. */
export function formatCommand(argv: string[]): string {
  const quoted = argv.map((a) => (/[\s"]/.test(a) ? JSON.stringify(a) : a));
  return ["claude", ...quoted].join(" ");
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

interface ResolvedArgs {
  runs: number;
  dryRun: boolean;
  outFile: string | null;
  taskFilter: string | null;
  targetFilter: string | null;
}

function parseArgs(args: string[]): ResolvedArgs {
  let runs = 5;
  let dryRun = false;
  let outFile: string | null = null;
  let taskFilter: string | null = null;
  let targetFilter: string | null = null;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--runs" && args[i + 1]) runs = Number.parseInt(args[++i], 10);
    else if (args[i] === "--dry-run") dryRun = true;
    else if (args[i] === "--out" && args[i + 1]) outFile = args[++i];
    else if (args[i] === "--task" && args[i + 1]) taskFilter = args[++i];
    else if (args[i] === "--target" && args[i + 1]) targetFilter = args[++i];
  }
  return { runs, dryRun, outFile, taskFilter, targetFilter };
}

async function main(): Promise<void> {
  const { runs, dryRun, outFile, taskFilter, targetFilter } = parseArgs(process.argv.slice(2));
  const targets = targetFilter
    ? AGENT_TARGETS.filter((t) => t.target === targetFilter)
    : AGENT_TARGETS;
  const tasks = taskFilter ? AGENT_TASKS.filter((t) => t.id === taskFilter) : AGENT_TASKS;

  const records: AgentRunRecord[] = [];

  for (const cfg of targets) {
    for (const task of tasks) {
      const argv = buildArgs(cfg, task);
      for (let i = 0; i < runs; i++) {
        if (dryRun) {
          console.log(`[dry-run] ${formatCommand(argv)}`);
          continue;
        }
        console.log(`Running: ${cfg.target} / ${task.id} / iteration ${i}`);
        const proc = Bun.spawn(["claude", ...argv], { stdout: "pipe", stderr: "pipe" });
        const stdout = await new Response(proc.stdout).text();
        await proc.exited;
        const record = parseResult(cfg.target, task.id, i, stdout);
        records.push(record);
        console.log(
          `  ok=${record.ok} durationMs=${record.durationMs} inputTokens=${record.inputTokens}`,
        );
      }
    }
  }

  if (outFile && !dryRun) {
    await Bun.write(outFile, JSON.stringify(records, null, 2));
    console.log(`\nAgent report written to: ${outFile}`);
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (err: unknown) {
    console.error("Agent benchmark failed:", err instanceof Error ? err.message : String(err));
    process.exitCode = 1;
  }
}
