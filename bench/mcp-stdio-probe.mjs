#!/usr/bin/env node
// Minimal MCP stdio probe: spawn a server, do initialize + tools/list over
// newline-delimited JSON-RPC, measure wall time from spawn to a successful
// tools/list response, and report the exact byte size of the tools array.
// No estimates: byte lengths are Buffer.byteLength of the real JSON text.
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

function usage() {
  console.error("usage: mcp-stdio-probe.mjs <command> [--arg ...] [--env KEY=VAL ...] [--timeout-ms N]");
  process.exit(2);
}

const argv = process.argv.slice(2);
if (argv.length === 0) usage();

let timeoutMs = 60000;
const cmdParts = [];
const env = { ...process.env };
let i = 0;
// Everything up to a literal "--" is the command+args; after that, flags.
const sepIdx = argv.indexOf("--");
const cmdArgs = sepIdx === -1 ? argv : argv.slice(0, sepIdx);
const flags = sepIdx === -1 ? [] : argv.slice(sepIdx + 1);
for (let j = 0; j < flags.length; j++) {
  if (flags[j] === "--env" && flags[j + 1]) {
    const [k, ...rest] = flags[++j].split("=");
    env[k] = rest.join("=");
  } else if (flags[j] === "--timeout-ms" && flags[j + 1]) {
    timeoutMs = Number.parseInt(flags[++j], 10);
  }
}
if (cmdArgs.length === 0) usage();
const [command, ...commandArgs] = cmdArgs;

function nowMs() {
  return Number(process.hrtime.bigint()) / 1e6;
}

async function probeOnce() {
  const spawnStart = nowMs();
  const child = spawn(command, commandArgs, { env, stdio: ["pipe", "pipe", "pipe"] });

  let settled = false;
  let stderrBuf = "";
  child.stderr.on("data", (d) => {
    stderrBuf += d.toString("utf8");
  });

  const rl = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const pending = new Map();
  let nextId = 1;

  function send(msg) {
    const line = JSON.stringify(msg) + "\n";
    child.stdin.write(line, "utf8");
  }

  function call(method, params) {
    return new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      send({ jsonrpc: "2.0", id, method, params: params ?? {} });
    });
  }

  rl.on("line", (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;
    let msg;
    try {
      msg = JSON.parse(trimmed);
    } catch {
      return; // ignore non-JSON lines (banners, logs that slip onto stdout)
    }
    if (msg.id !== undefined && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(JSON.stringify(msg.error)));
      else resolve(msg.result);
    }
  });

  const result = { ok: false, error: null, startupMs: null, toolCount: null, toolsBytes: null, bytesPerTool: null, toolNames: null };

  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error(`timed out after ${timeoutMs}ms`)), timeoutMs),
  );

  try {
    await Promise.race([
      (async () => {
        await call("initialize", {
          protocolVersion: "2024-11-05",
          capabilities: {},
          clientInfo: { name: "turbofig-bench-probe", version: "0.0.1" },
        });
        send({ jsonrpc: "2.0", method: "notifications/initialized", params: {} });
        const toolsResult = await call("tools/list", {});
        const elapsed = nowMs() - spawnStart;
        const tools = Array.isArray(toolsResult?.tools) ? toolsResult.tools : [];
        const serialized = JSON.stringify(tools);
        const bytes = Buffer.byteLength(serialized, "utf8");
        result.ok = true;
        result.startupMs = elapsed;
        result.toolCount = tools.length;
        result.toolsBytes = bytes;
        result.bytesPerTool = tools.length > 0 ? bytes / tools.length : null;
        result.toolNames = tools.map((t) => t.name);
      })(),
      timeout,
    ]);
  } catch (err) {
    result.ok = false;
    result.error = err instanceof Error ? err.message : String(err);
    result.stderr = stderrBuf.trim().slice(0, 2000);
  } finally {
    settled = true;
    rl.close();
    child.kill("SIGTERM");
  }

  return result;
}

const out = await probeOnce();
process.stdout.write(JSON.stringify(out) + "\n");
process.exit(out.ok ? 0 : 1);
