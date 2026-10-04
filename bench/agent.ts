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
 * project CLAUDE.md or skill leaks into context, `--tools ""` so none of
 * Claude Code's built-in tools (Bash, Write, Edit, WebFetch, etc.) load at
 * all, and `--allowedTools "mcp__<server>__*"` so the session can reach only
 * that target's own MCP tools by name. This script never uses
 * `bypassPermissions`: that mode only skips the permission prompt, it does
 * not remove any built-in tool from the session, so an unattended run under
 * it could still reach Bash or Write and go around the target entirely. With
 * `--tools ""` there is nothing left to bypass. The "trivial" task ("reply
 * OK", no tool call) isolates the fixed tool-schema cost as a pure
 * input-token difference between targets, because no other work runs.
 *
 * `ok` is never read from the claimed JSON result alone: it also requires
 * the spawned process to exit 0. The "create-frame" task goes one step
 * further and verifies through the target's own tool that the node it
 * claims to have created actually exists, then deletes it, so one
 * iteration's state never leaks into the next and a hallucinated "done"
 * claim is caught rather than counted as a success.
 *
 * This script never calls `claude -p` with a real prompt on its own.
 * `bun bench/agent.ts --dry-run` prints the exact commands it would run
 * without spawning them. A real run spends Claude Code usage, so the owner
 * runs it deliberately: `bun bench/agent.ts --runs 5 --out agent-report.json`.
 *
 * Every flag here was checked against `claude --help` before use: -p,
 * --mcp-config, --strict-mcp-config, --bare, --tools, --allowedTools,
 * --output-format are all real, documented flags.
 */

import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { defaultMcpUrl } from "./targets.js";
import { consoleMcpAdapter, mcpTransport, turbofigMcpAdapter } from "./transports.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

/** The exact node name the "create-frame" task is told to use. Verification
 * and cleanup both search for this name, through the target's own tool, so
 * a claimed result is never trusted without the target confirming it. */
const CREATE_FRAME_NODE_NAME = "Agent Bench";

/** Code run through the target's own execute tool after a "create-frame"
 * task: find every node named CREATE_FRAME_NODE_NAME on any page, delete
 * it, and report how many were found. Verification and cleanup in one call
 * so a run never leaves state behind for the next run to trip over. */
const VERIFY_AND_CLEANUP_CODE = `
await figma.loadAllPagesAsync();
const matches = [];
function walk(node) {
  if (node.name === ${JSON.stringify(CREATE_FRAME_NODE_NAME)}) matches.push(node);
  if ("children" in node) for (const child of node.children) walk(child);
}
for (const page of figma.root.children) walk(page);
for (const node of matches) node.remove();
return { found: matches.length };
`.trim();

/** Verify a "create-frame" task actually produced the named node, through
 * the target's own MCP tool (never trust the agent's claimed result text),
 * then delete it so the next iteration starts clean. Returns null when the
 * target's own call fails, so the caller can report that separately from a
 * genuine "node not found". */
async function verifyAndCleanupCreateFrame(cfg: TargetAgentConfig): Promise<boolean | null> {
  const adapter = cfg.target === "turbofig-mcp" ? turbofigMcpAdapter : consoleMcpAdapter;
  const transport = mcpTransport(defaultMcpUrl(cfg.target), adapter, 10_000);
  try {
    const { ok, result } = await transport.submit({ op: "execute", code: VERIFY_AND_CLEANUP_CODE });
    if (!ok) return null;
    const found = (result as { found?: number } | undefined)?.found ?? 0;
    return found > 0;
  } catch {
    return null;
  } finally {
    await transport.close?.();
  }
}

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
  /** Raw usage.input_tokens only (the uncached portion). Most of the real
   * cost of a tool-heavy schema sits in the cache fields below, never here
   * alone, so this field by itself understates the per-session cost. */
  inputTokens: number | null;
  outputTokens: number | null;
  cacheCreationTokens: number | null;
  cacheReadTokens: number | null;
  /** inputTokens + cacheCreationTokens + cacheReadTokens. This is the real
   * per-session input cost; report this, not inputTokens alone, when
   * comparing targets. Null only when usage is missing entirely. */
  totalInputTokens: number | null;
  totalCostUsd: number | null;
  /** The spawned `claude` process's exit code. ok requires 0 here, not only
   * a non-error JSON payload: the CLI usage schema does not promise 0 stdout
   * parse failures always carry a non-zero exit. */
  exitCode: number | null;
  /** Trimmed stderr text, for diagnosis. Non-empty stderr alone does not
   * flip ok to false (a successful run can still print a benign warning). */
  stderr: string | null;
  /** Only set for a task with a verification step (currently "create-frame").
   * true: the target itself confirmed the created node exists and it was
   * deleted. false: the node was never found, so the agent's claimed result
   * was not real. null: this task has no verification step. */
  verified: boolean | null;
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
    "--tools",
    "",
    "--allowedTools",
    `mcp__${cfg.serverName}__*`,
    "--output-format",
    "json",
  ];
}

/** The spawned process's exit code and stderr text, passed in alongside its
 * stdout so parseResult can judge success from the whole process, not only
 * the JSON payload. */
export interface ProcessOutcome {
  exitCode: number | null;
  stderr: string;
}

/** Parse one `claude -p ... --output-format json` stdout string into a
 * record. `proc`, when given, folds the process exit code and stderr into
 * `ok`: a zero-but-malformed exit or a non-zero exit both count as failed,
 * even when stdout happens to parse as a non-error JSON payload. */
export function parseResult(
  target: string,
  task: string,
  iteration: number,
  stdout: string,
  proc?: ProcessOutcome,
): AgentRunRecord {
  const exitCode = proc?.exitCode ?? null;
  const stderr = proc && proc.stderr.trim().length > 0 ? proc.stderr.trim() : null;
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
      totalInputTokens: null,
      totalCostUsd: null,
      exitCode,
      stderr,
      verified: null,
      error: `failed to parse claude JSON output: ${err instanceof Error ? err.message : String(err)}`,
    };
  }
  const usage = parsed.usage;
  const exitOk = exitCode === null || exitCode === 0;
  const ok = parsed.is_error !== true && exitOk;
  return {
    target,
    task,
    iteration,
    ok,
    durationMs: parsed.duration_ms ?? null,
    numTurns: parsed.num_turns ?? null,
    inputTokens: usage?.input_tokens ?? null,
    outputTokens: usage?.output_tokens ?? null,
    cacheCreationTokens: usage?.cache_creation_input_tokens ?? null,
    cacheReadTokens: usage?.cache_read_input_tokens ?? null,
    totalInputTokens: usage
      ? (usage.input_tokens ?? 0) +
        (usage.cache_creation_input_tokens ?? 0) +
        (usage.cache_read_input_tokens ?? 0)
      : null,
    totalCostUsd: parsed.total_cost_usd ?? null,
    exitCode,
    stderr,
    verified: null,
    error: !ok
      ? (parsed.result ?? (exitOk ? "unknown error" : `claude exited with code ${exitCode}`))
      : null,
  };
}

/** Render one invocation as a copy-pasteable shell command, for --dry-run. */
export function formatCommand(argv: string[]): string {
  const quoted = argv.map((a) => (a === "" || /[\s"]/.test(a) ? JSON.stringify(a) : a));
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
        const stderr = await new Response(proc.stderr).text();
        const exitCode = await proc.exited;
        const record = parseResult(cfg.target, task.id, i, stdout, { exitCode, stderr });

        if (task.id === "create-frame") {
          const verified = await verifyAndCleanupCreateFrame(cfg);
          record.verified = verified;
          if (record.ok && verified === false) {
            record.ok = false;
            record.error = `${cfg.target} reports no node named "${CREATE_FRAME_NODE_NAME}" exists; the claimed result was not real`;
          } else if (verified === null) {
            console.log(
              `  warning: could not verify/clean up "${CREATE_FRAME_NODE_NAME}" via ${cfg.target}`,
            );
          }
        }

        records.push(record);
        console.log(
          `  ok=${record.ok} durationMs=${record.durationMs} totalInputTokens=${record.totalInputTokens} verified=${record.verified}`,
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
