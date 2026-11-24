/**
 * `turbofig js` against the old shell recipe and raw MCP, on one mock-plugin
 * daemon, simple execute job, wall time measured from this parent process
 * (including process spawn). Re-run: `bun bench/js-vs-recipe.ts [runs]`
 * after `cargo build --release`.
 *
 * It uses a temp HOME and free ports. It never touches ~/.turbofig or the
 * live ports 18846/18847. The mock plugin runs in a child of this script
 * (`--mock-plugin`) so its work does not share this timing loop.
 * Output: a markdown report in bench/results/js-vs-recipe-<date>.md.
 */

import { createHmac } from "node:crypto";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const BIN = resolve(import.meta.dir, "../target/release/turbofig");
const JOB = JSON.stringify({ op: "execute", code: 'return "bench";' });

// ---- mock plugin (child mode) -------------------------------------------

function hmacProof(token: string, label: string, msg: string): string {
  return createHmac("sha256", token)
    .update(label + msg)
    .digest("hex");
}

function runMockPlugin(wsPort: number, token: string): void {
  const ws = new WebSocket(`ws://127.0.0.1:${wsPort}/?nonce=00112233445566778899aabbccddeeff`);
  ws.onmessage = (ev) => {
    const frame = JSON.parse(String(ev.data));
    if (frame.type === "CHALLENGE") {
      ws.send(
        JSON.stringify({
          type: "AUTH",
          proof: hmacProof(token, "turbofig-ws-plugin:", frame.nonce),
        }),
      );
      ws.send(JSON.stringify({ type: "FILE_INFO", fileKey: "BenchFile", name: "Bench" }));
    } else if (frame.type === "EXECUTE") {
      ws.send(
        JSON.stringify({
          type: "RESULT",
          requestId: frame.requestId,
          ok: true,
          result: { marker: "bench", id: "12:34" },
        }),
      );
    }
  };
  ws.onclose = () => process.exit(0);
}

// ---- bench (parent mode) -------------------------------------------------

function freePort(): Promise<number> {
  return new Promise((res) => {
    const s = createServer().listen(0, "127.0.0.1", () => {
      const p = (s.address() as { port: number }).port;
      s.close(() => res(p));
    });
  });
}

function stats(ms: number[]): { median: number; p95: number } {
  const s = [...ms].sort((a, b) => a - b);
  return {
    median: s[Math.floor(s.length / 2)],
    p95: s[Math.min(s.length - 1, Math.ceil(s.length * 0.95) - 1)],
  };
}

async function timed(
  cmd: string[],
  env: Record<string, string>,
  stdin?: string,
): Promise<{ ms: number; out: string }> {
  const t = performance.now();
  const p = Bun.spawn(cmd, {
    env,
    stdin: stdin === undefined ? "ignore" : new TextEncoder().encode(stdin),
    stdout: "pipe",
    stderr: "pipe",
  });
  const out = await new Response(p.stdout).text();
  await p.exited;
  return { ms: performance.now() - t, out };
}

/** The recipe agents typed before `turbofig js`, byte for byte. */
function recipe(bridge: string): string {
  return [
    `B=${bridge};I=$RANDOM$RANDOM;X=$B/inbox/$I.json;cat >$X.tmp <<'EOF'`,
    JOB,
    "EOF",
    "mv $X.tmp $X;O=$B/outbox/$I.json;for i in $(seq 1750);do [ -s $O ]&&break;sleep .02;done;cat $O",
  ].join("\n");
}

async function main(): Promise<void> {
  const runs = Number(process.argv[2] ?? 50);
  const home = mkdtempSync(join(tmpdir(), "tf-bench-"));
  const mcp = await freePort();
  const ws = await freePort();
  const env: Record<string, string> = {
    PATH: process.env.PATH ?? "",
    HOME: home,
    TURBOFIG_MCP_PORT: String(mcp),
    TURBOFIG_WS_PORT: String(ws),
    TURBOFIG_BRIDGE_DIR: home,
  };
  let plugin: ReturnType<typeof Bun.spawn> | undefined;
  try {
    // The first `turbofig js` starts the daemon.
    await timed([BIN, "js"], env, '{"op":"status"}');
    const token = readFileSync(join(home, "token"), "utf8").trim();
    plugin = Bun.spawn(["bun", import.meta.path, "--mock-plugin", String(ws), token], {
      stdout: "ignore",
      stderr: "inherit",
    });
    for (let i = 0; ; i++) {
      const r = await timed([BIN, "js"], env, 'return "bench";');
      if (r.out.includes('"ok":true')) break;
      if (i > 100) throw new Error(`plugin never routable: ${r.out}`);
      await Bun.sleep(100);
    }

    const clean = () => {
      for (const d of ["inbox", "outbox"]) {
        for (const f of readdirSync(join(home, d))) {
          if (!f.startsWith(".")) rmSync(join(home, d, f), { force: true });
        }
      }
    };
    const check = (label: string, out: string) => {
      if (!out.includes("bench")) throw new Error(`${label}: bad output ${out}`);
    };

    const rows: { name: string; ms: number[] }[] = [];
    const series = async (name: string, one: () => Promise<number>) => {
      for (let i = 0; i < 3; i++) await one(); // warm-up, not counted
      const ms: number[] = [];
      for (let i = 0; i < runs; i++) ms.push(await one());
      rows.push({ name, ms });
      clean();
    };

    const trueMs: number[] = [];
    for (let i = 0; i < runs; i++) trueMs.push((await timed(["/usr/bin/true"], env)).ms);

    for (const sh of ["/bin/zsh", "/bin/bash"]) {
      await series(`old recipe, ${sh} -c`, async () => {
        const r = await timed([sh, "-c", recipe(home)], env);
        check(sh, r.out);
        clean();
        return r.ms;
      });
    }
    await series("turbofig js", async () => {
      const r = await timed([BIN, "js"], env, JOB);
      check("js", r.out);
      return r.ms;
    });
    await series("turbofig js via /bin/zsh -c", async () => {
      const r = await timed(["/bin/zsh", "-c", `${BIN} js`], env, JOB);
      check("zsh js", r.out);
      return r.ms;
    });

    // Raw MCP: initialize once, time only tools/call.
    const url = `http://127.0.0.1:${mcp}/mcp`;
    const hdr = {
      Accept: "application/json, text/event-stream",
      "Content-Type": "application/json",
      Authorization: `Bearer ${token}`,
    };
    const init = await fetch(url, {
      method: "POST",
      headers: hdr,
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2025-03-26",
          clientInfo: { name: "bench", version: "0" },
          capabilities: {},
        },
      }),
    });
    const sid = init.headers.get("mcp-session-id") ?? "";
    await init.text();
    const h2 = { ...hdr, "mcp-session-id": sid };
    await (
      await fetch(url, {
        method: "POST",
        headers: h2,
        body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized", params: {} }),
      })
    ).text();
    let rid = 2;
    await series("raw MCP tools/call (HTTP, no process spawn)", async () => {
      const t = performance.now();
      const res = await fetch(url, {
        method: "POST",
        headers: h2,
        body: JSON.stringify({
          jsonrpc: "2.0",
          id: rid++,
          method: "tools/call",
          params: { name: "turbofig_execute", arguments: { code: 'return "bench";' } },
        }),
      });
      const body = await res.text();
      const ms = performance.now() - t;
      check("mcp", body);
      return ms;
    });

    const f = (n: number) => n.toFixed(1);
    const date = new Date().toISOString().slice(0, 10);
    const t0 = stats(trueMs);
    const lines = [
      `# turbofig js vs the old recipe vs raw MCP (${date})`,
      "",
      `Mock plugin answers a simple execute job. ${runs} runs each (3 warm-up runs not counted). Wall time from this parent process, process spawn included. Release build of this checkout. Machine: ${process.platform} ${process.arch}, bun ${Bun.version}.`,
      "",
      `Process start alone (\`/usr/bin/true\`): median ${f(t0.median)} ms, p95 ${f(t0.p95)} ms.`,
      "",
      "| Path | median ms | p95 ms |",
      "|---|---|---|",
      ...rows.map((r) => {
        const s = stats(r.ms);
        return `| ${r.name} | ${f(s.median)} | ${f(s.p95)} |`;
      }),
      "",
      "Notes:",
      "- The old recipe polls every 20 ms (`sleep .02`), so it adds up to 20 ms of wait plus the process starts of `seq`, `sleep` and `cat`.",
      "- The raw MCP row has no process spawn and keeps one session. It is the transport floor of MCP mode, not what an agent pays per call (an agent adds model turn time and tool schema tokens).",
      "- Re-run: `bun bench/js-vs-recipe.ts [runs]` after `cargo build --release`.",
      "",
    ];
    mkdirSync(resolve(import.meta.dir, "results"), { recursive: true });
    const outFile = resolve(import.meta.dir, `results/js-vs-recipe-${date}.md`);
    writeFileSync(outFile, lines.join("\n"));
    console.log(lines.join("\n"));
    console.log(`written ${outFile}`);
  } finally {
    plugin?.kill();
    try {
      const token = readFileSync(join(home, "token"), "utf8").trim();
      await fetch(`http://127.0.0.1:${mcp}/control`, {
        method: "POST",
        headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
        body: JSON.stringify({ action: "stop" }),
      });
    } catch {}
    await Bun.sleep(500);
    rmSync(home, { recursive: true, force: true });
  }
}

if (process.argv[2] === "--mock-plugin") {
  runMockPlugin(Number(process.argv[3]), process.argv[4] ?? "");
} else {
  await main();
}
