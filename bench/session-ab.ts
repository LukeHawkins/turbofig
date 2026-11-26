#!/usr/bin/env bun
/**
 * session-ab: A/B benchmark of real agent design sessions between turbofig versions.
 * Drives headless Claude Code against a live daemon + Figma plugin through the file bridge
 * (inbox/outbox). No HTTP. See the "session-ab" section in bench/README.md.
 */
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import {
  appendFile,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  unlink,
  writeFile,
} from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";

const LANDING4 =
  "In the connected Figma file, build a simple landing page in 4 steps on the current page: " +
  "1) hero with a title, a subtitle and a button; 2) a features row with 3 cards; " +
  "3) pricing with 2 plans; 4) a footer. After each step, take a screenshot of the page frame " +
  "and look at it to check it before you continue. When all 4 steps are done, reply DONE.";

interface Args {
  label: string;
  promptRef?: string;
  promptFile?: string;
  mcpConfig?: string;
  mcpServer?: string;
  bin: string;
  runs: number;
  model: string;
  maxTurns: number;
  bridgeDir: string;
  brief: string;
  out: string;
  dryRun: boolean;
  compare?: [string, string];
}

function parseArgs(argv: string[]): Args {
  const a: Args = {
    label: "",
    bin: "/Users/l.hawkins/Development/turbofig/target/release/turbofig",
    runs: 3,
    model: "claude-sonnet-5-5",
    maxTurns: 40,
    bridgeDir: join(homedir(), ".turbofig"),
    brief: "landing4",
    out: "/tmp/tf-session-ab",
    dryRun: false,
  };
  const need = (i: number, f: string): string => {
    const v = argv[i + 1];
    if (v === undefined) throw new Error(`Missing value for ${f}`);
    return v;
  };
  for (let i = 0; i < argv.length; i++) {
    const f = argv[i];
    if (f === "--label") a.label = need(i++, f);
    else if (f === "--prompt-ref") a.promptRef = need(i++, f);
    else if (f === "--prompt-file") a.promptFile = need(i++, f);
    else if (f === "--mcp-config") a.mcpConfig = need(i++, f);
    else if (f === "--mcp-server") a.mcpServer = need(i++, f);
    else if (f === "--bin") a.bin = need(i++, f);
    else if (f === "--runs") a.runs = Number(need(i++, f));
    else if (f === "--model") a.model = need(i++, f);
    else if (f === "--max-turns") a.maxTurns = Number(need(i++, f));
    else if (f === "--bridge-dir") a.bridgeDir = need(i++, f);
    else if (f === "--brief") a.brief = need(i++, f);
    else if (f === "--out") a.out = need(i++, f);
    else if (f === "--dry-run") a.dryRun = true;
    else if (f === "--compare") {
      a.compare = [need(i, f), argv[i + 2] ?? ""];
      i += 2;
    } else throw new Error(`Unknown argument: ${f}`);
  }
  a.bridgeDir = resolve(a.bridgeDir.replace(/^~/, homedir()));
  if (!a.compare) {
    if (!a.label) throw new Error("--label is required");
    if (!!a.mcpConfig !== !!a.mcpServer)
      throw new Error("--mcp-config and --mcp-server must be used together");
    if (!a.mcpConfig && !a.promptRef && !a.promptFile)
      throw new Error("--prompt-ref or --prompt-file is required");
    if (a.mcpConfig) a.mcpConfig = resolve(a.mcpConfig.replace(/^~/, homedir()));
    if (!Number.isInteger(a.runs) || a.runs < 1)
      throw new Error("--runs must be a positive integer");
  }
  return a;
}

// ---------- prompt ----------

async function loadTemplate(a: Args): Promise<string> {
  if (a.promptFile) return readFile(a.promptFile, "utf8");
  const p = Bun.spawnSync(["git", "show", `${a.promptRef}:prompts/agent-prompt.txt`], {
    cwd: resolve(import.meta.dir, ".."),
  });
  if (p.exitCode !== 0) throw new Error(`git show failed: ${p.stderr.toString().trim()}`);
  return p.stdout.toString();
}

function render(tpl: string, fileKey: string, bridgeDir: string, bin: string): string {
  const vals: Record<string, string> = {
    FILES_LINE: "",
    FILE_KEY: fileKey,
    FILE_KEY_HINT: "",
    BRIDGE_DIR: bridgeDir,
    MCP_PORT: "18846",
    TURBOFIG_BIN: bin,
    BRIDGE_FLAG: "",
  };
  const out = tpl.replace(/\{\{([A-Z_]+)\}\}/g, (m, k: string) =>
    k in vals ? (vals[k] as string) : m,
  );
  const left = out.match(/\{\{[^}]*\}\}/g);
  if (left) throw new Error(`Unreplaced placeholders: ${[...new Set(left)].join(", ")}`);
  return out;
}

async function loadBrief(brief: string): Promise<string> {
  if (brief === "landing4") return LANDING4;
  if (existsSync(brief)) return (await readFile(brief, "utf8")).trim();
  throw new Error(`Unknown brief: ${brief} (use landing4 or a file path)`);
}

// ---------- file bridge ----------

type Reply = Record<string, unknown>;

async function bridgeJob(bridgeDir: string, payload: Record<string, unknown>): Promise<Reply> {
  const id = randomUUID();
  const inbox = join(bridgeDir, "inbox");
  const tmp = join(inbox, `${id}.json.tmp`);
  const job = join(inbox, `${id}.json`);
  const outPath = join(bridgeDir, "outbox", `${id}.json`);
  try {
    await writeFile(tmp, JSON.stringify(payload));
    await rename(tmp, job);
    const end = Date.now() + 35_000;
    while (Date.now() < end) {
      if (existsSync(outPath)) {
        const text = await readFile(outPath, "utf8");
        return JSON.parse(text) as Reply;
      }
      await Bun.sleep(20);
    }
    throw new Error(`Bridge timeout for job ${id}`);
  } finally {
    await unlink(tmp).catch(() => {});
    await unlink(job).catch(() => {});
    await unlink(outPath).catch(() => {});
  }
}

function findFileKeys(v: unknown, acc: Set<string>): void {
  if (Array.isArray(v)) for (const x of v) findFileKeys(x, acc);
  else if (v && typeof v === "object") {
    const o = v as Record<string, unknown>;
    if (typeof o.fileKey === "string" && o.fileKey) acc.add(o.fileKey);
    for (const x of Object.values(o)) findFileKeys(x, acc);
  }
}

async function connectedFileKey(bridgeDir: string): Promise<string> {
  const r = await bridgeJob(bridgeDir, { op: "status" });
  const keys = new Set<string>();
  findFileKeys(r, keys);
  if (keys.size !== 1) {
    throw new Error(
      `Exactly one connected Figma file is required, found ${keys.size}. ` +
        "Open the turbofig plugin in one file only, then retry.",
    );
  }
  return [...keys][0] as string;
}

async function exec(bridgeDir: string, fileKey: string, code: string): Promise<unknown> {
  const r = await bridgeJob(bridgeDir, { op: "execute", fileKey, code });
  if (r.ok === false)
    throw new Error(`execute failed: ${JSON.stringify(r.error ?? r).slice(0, 300)}`);
  return "result" in r ? r.result : r;
}

// ---------- stream parsing ----------

type Kind = "turbofig js" | "--shot" | "--guide" | "inbox/outbox recipe" | "other";

function classify(cmd: string): Kind {
  // The binary name followed by whitespace and `js`. A `.turbofig/inbox/...`
  // path is followed by `/`, so 0.1.0's file writes never count as js calls.
  const js = /(^|[\s/'"])turbofig['"]?\s+js\b/.test(cmd);
  if (js && cmd.includes("--guide")) return "--guide";
  if (js && cmd.includes("--shot")) return "--shot";
  if (js) return "turbofig js";
  if (/\b(inbox|outbox)\b/.test(cmd)) return "inbox/outbox recipe";
  return "other";
}

interface RunRecord {
  label: string;
  run: number;
  model: string;
  brief: string;
  ok: boolean;
  done: boolean;
  topLevelNodes: number;
  inputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  outputTokens: number;
  totalTokens: number;
  costUsd: number | null;
  durationMs: number;
  turns: number;
  isError: boolean;
  toolCalls: number;
  failedToolCalls: number;
  toolKinds: Record<string, number>;
  toolNames: Record<string, number>;
  resultText: string;
  error?: string;
}

function parseStream(stdout: string, mcpServer?: string) {
  const uses = new Map<string, { name: string; kind?: string }>();
  const kinds: Record<string, number> = {};
  const names: Record<string, number> = {};
  let calls = 0;
  let failed = 0;
  let result: Record<string, unknown> | undefined;
  for (const line of stdout.split("\n")) {
    if (!line.trim()) continue;
    let m: Record<string, unknown>;
    try {
      m = JSON.parse(line);
    } catch {
      continue;
    }
    if (m.type === "result") result = m;
    const content = (m.message as { content?: unknown } | undefined)?.content;
    if (!Array.isArray(content)) continue;
    for (const c of content as Record<string, unknown>[]) {
      if (c.type === "tool_use") {
        calls++;
        const name = String(c.name);
        names[name] = (names[name] ?? 0) + 1;
        let kind: string | undefined;
        if (mcpServer) {
          const prefix = `mcp__${mcpServer}__`;
          kind = name.startsWith(prefix) ? name.slice(prefix.length) : "other";
          kinds[kind] = (kinds[kind] ?? 0) + 1;
        } else if (name === "Bash") {
          kind = classify(String((c.input as { command?: string } | undefined)?.command ?? ""));
          kinds[kind] = (kinds[kind] ?? 0) + 1;
        }
        uses.set(String(c.id), { name, kind });
      } else if (c.type === "tool_result" && c.is_error === true) failed++;
    }
  }
  return { result, calls, failed, kinds, names };
}

// ---------- one run ----------

function claudeArgs(a: Args, prompt: string): string[] {
  return [
    "-p",
    prompt,
    "--model",
    a.model,
    "--max-turns",
    String(a.maxTurns),
    "--output-format",
    "stream-json",
    "--verbose",
    "--permission-mode",
    "default",
    "--setting-sources",
    "local",
    "--strict-mcp-config",
    "--mcp-config",
    a.mcpConfig ?? '{"mcpServers":{}}',
    "--allowedTools",
    a.mcpServer ? `mcp__${a.mcpServer}__*,Read` : "Bash,Read",
  ];
}

async function runClaude(a: Args, prompt: string, cwd: string): Promise<string> {
  return new Promise((res, rej) => {
    const p = spawn("claude", claudeArgs(a, prompt), {
      cwd,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const chunks: Buffer[] = [];
    p.stdout.on("data", (d: Buffer) => chunks.push(d));
    p.stderr.on("data", () => {});
    p.on("error", rej);
    p.on("close", () => res(Buffer.concat(chunks).toString("utf8")));
  });
}

async function oneRun(a: Args, n: number, fileKey: string, prompt: string): Promise<RunRecord> {
  const rec: RunRecord = {
    label: a.label,
    run: n,
    model: a.model,
    brief: a.brief,
    ok: false,
    done: false,
    topLevelNodes: 0,
    inputTokens: 0,
    cacheCreationTokens: 0,
    cacheReadTokens: 0,
    outputTokens: 0,
    totalTokens: 0,
    costUsd: null,
    durationMs: 0,
    turns: 0,
    isError: false,
    toolCalls: 0,
    failedToolCalls: 0,
    toolKinds: {},
    toolNames: {},
    resultText: "",
  };
  const pageName = `bench-${a.label}-${n}`;
  let prevPageId: string | undefined;
  let pageId: string | undefined;
  let tmp: string | undefined;
  const started = Date.now();
  try {
    const created = (await exec(
      a.bridgeDir,
      fileKey,
      `const prev = figma.currentPage.id; const page = figma.createPage(); page.name = ${JSON.stringify(pageName)}; await figma.setCurrentPageAsync(page); return { prev, page: page.id };`,
    )) as { prev: string; page: string };
    prevPageId = created.prev;
    pageId = created.page;
    tmp = await mkdtemp(join(tmpdir(), "tf-session-"));
    const stdout = await runClaude(a, prompt, tmp);
    const s = parseStream(stdout, a.mcpServer);
    const r = s.result ?? {};
    const u = (r.usage ?? {}) as Record<string, number>;
    rec.inputTokens = u.input_tokens ?? 0;
    rec.cacheCreationTokens = u.cache_creation_input_tokens ?? 0;
    rec.cacheReadTokens = u.cache_read_input_tokens ?? 0;
    rec.outputTokens = u.output_tokens ?? 0;
    rec.totalTokens =
      rec.inputTokens + rec.cacheCreationTokens + rec.cacheReadTokens + rec.outputTokens;
    rec.costUsd = typeof r.total_cost_usd === "number" ? r.total_cost_usd : null;
    rec.durationMs = typeof r.duration_ms === "number" ? r.duration_ms : Date.now() - started;
    rec.turns = typeof r.num_turns === "number" ? r.num_turns : 0;
    rec.isError = r.is_error === true || !s.result;
    rec.resultText = String(r.result ?? "");
    rec.toolCalls = s.calls;
    rec.failedToolCalls = s.failed;
    rec.toolKinds = s.kinds;
    rec.toolNames = s.names;
    rec.done = /\bDONE\b/.test(rec.resultText);
    const count = (await exec(
      a.bridgeDir,
      fileKey,
      `const p = await figma.getNodeByIdAsync(${JSON.stringify(pageId)}); return p ? p.children.length : 0;`,
    )) as number;
    rec.topLevelNodes = Number(count) || 0;
    rec.ok = rec.done && rec.topLevelNodes >= 1 && !rec.isError;
  } catch (e) {
    rec.error = e instanceof Error ? e.message : String(e);
    if (!rec.durationMs) rec.durationMs = Date.now() - started;
  } finally {
    if (pageId) {
      await exec(
        a.bridgeDir,
        fileKey,
        `const prev = ${JSON.stringify(prevPageId ?? "")} ? await figma.getNodeByIdAsync(${JSON.stringify(prevPageId ?? "")}) : null; if (prev) await figma.setCurrentPageAsync(prev); const p = await figma.getNodeByIdAsync(${JSON.stringify(pageId)}); if (p) p.remove(); return true;`,
      ).catch((e) => {
        rec.error =
          `${rec.error ?? ""} cleanup failed: ${e instanceof Error ? e.message : e}`.trim();
      });
    }
    if (tmp) await rm(tmp, { recursive: true, force: true }).catch(() => {});
  }
  return rec;
}

// ---------- summary ----------

function median(xs: number[]): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((x, y) => x - y);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? (s[m] as number) : ((s[m - 1] as number) + (s[m] as number)) / 2;
}

const METRICS: [string, (r: RunRecord) => number | null, number][] = [
  ["total tokens", (r) => r.totalTokens, 0],
  ["output tokens", (r) => r.outputTokens, 0],
  ["cost USD", (r) => r.costUsd, 4],
  ["time s", (r) => r.durationMs / 1000, 1],
  ["turns", (r) => r.turns, 1],
  ["tool calls", (r) => r.toolCalls, 1],
  ["failed tool calls", (r) => r.failedToolCalls, 1],
  ["success", (r) => (r.ok ? 1 : 0), 2],
];

function medianOf(rs: RunRecord[], f: (r: RunRecord) => number | null): number | null {
  const v = rs.map(f).filter((x): x is number => x !== null);
  return v.length ? median(v) : null;
}

function summary(label: string, rs: RunRecord[]): string {
  const L = [
    `# session-ab: ${label}`,
    "",
    `Runs: ${rs.length}. Model: ${rs[0]?.model ?? "?"}. Brief: ${rs[0]?.brief ?? "?"}.`,
    "",
  ];
  L.push(
    "| run | total tok | out tok | cost USD | time s | turns | tools | failed | nodes | success |",
  );
  L.push("|---|---|---|---|---|---|---|---|---|---|");
  for (const r of rs) {
    L.push(
      `| ${r.run} | ${r.totalTokens} | ${r.outputTokens} | ${r.costUsd?.toFixed(4) ?? "n/a"} | ${(r.durationMs / 1000).toFixed(1)} | ${r.turns} | ${r.toolCalls} | ${r.failedToolCalls} | ${r.topLevelNodes} | ${r.ok ? "yes" : "no"} |`,
    );
  }
  L.push("", "| median | value |", "|---|---|");
  for (const [name, f, d] of METRICS) {
    const m = medianOf(rs, f);
    L.push(`| ${name} | ${m === null ? "n/a" : m.toFixed(d)} |`);
  }
  L.push("", "Time is noisy when the machine is busy.");
  return L.join("\n");
}

async function readJsonl(path: string): Promise<RunRecord[]> {
  const t = await readFile(path, "utf8");
  return t
    .split("\n")
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l) as RunRecord);
}

async function compare(pa: string, pb: string): Promise<void> {
  const [a, b] = [await readJsonl(pa), await readJsonl(pb)];
  console.log(`| metric (median) | A ${a[0]?.label ?? pa} | B ${b[0]?.label ?? pb} | B/A |`);
  console.log("|---|---|---|---|");
  for (const [name, f, d] of METRICS) {
    const x = medianOf(a, f);
    const y = medianOf(b, f);
    const ratio = x && y !== null ? (y / x).toFixed(2) : "n/a";
    console.log(`| ${name} | ${x?.toFixed(d) ?? "n/a"} | ${y?.toFixed(d) ?? "n/a"} | ${ratio} |`);
  }
}

// ---------- main ----------

async function main(): Promise<void> {
  const a = parseArgs(process.argv.slice(2));
  if (a.compare) {
    if (!a.compare[1]) throw new Error("--compare needs two jsonl files");
    return compare(a.compare[0], a.compare[1]);
  }
  const brief = await loadBrief(a.brief);
  const tpl = a.mcpConfig && !a.promptRef && !a.promptFile ? "" : await loadTemplate(a);
  const fileKey = a.dryRun ? "DRYRUN_FILE_KEY" : await connectedFileKey(a.bridgeDir);
  const setup = tpl ? render(tpl, fileKey, a.bridgeDir, a.bin) : "";
  const prompt = setup ? `${setup}\n\n${brief}` : brief;

  if (a.dryRun) {
    console.log("DRY RUN: nothing is run, the bridge is not touched.\n");
    console.log("--- rendered prompt ---\n" + prompt + "\n--- end prompt ---\n");
    const shown = claudeArgs(a, "<PROMPT>").map((x) => (/[\s{"]/.test(x) ? JSON.stringify(x) : x));
    console.log(`claude ${shown.join(" ")}`);
    console.log(`cwd: fresh empty temp dir. Runs: ${a.runs}. Out: ${a.out}/${a.label}.jsonl`);
    return;
  }

  await mkdir(a.out, { recursive: true });
  const jsonl = join(a.out, `${a.label}.jsonl`);
  const recs: RunRecord[] = [];
  for (let n = 1; n <= a.runs; n++) {
    console.log(`run ${n}/${a.runs} ...`);
    const rec = await oneRun(a, n, fileKey, prompt);
    recs.push(rec);
    await appendFile(jsonl, `${JSON.stringify(rec)}\n`);
    console.log(`  ok=${rec.ok} tokens=${rec.totalTokens} turns=${rec.turns} ${rec.error ?? ""}`);
  }
  const md = summary(a.label, recs);
  await writeFile(join(a.out, `summary-${a.label}.md`), `${md}\n`);
  console.log(`\n${md}`);
}

main().catch((e) => {
  console.error(e instanceof Error ? e.message : e);
  process.exit(1);
});
