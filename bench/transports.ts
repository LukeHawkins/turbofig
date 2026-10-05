/**
 * Transports: one per target, plus a stub for --dry-run.
 *
 * Every transport reports exact wire bytes in and out (not an estimate), and
 * times itself the same way: the clock starts right before the request is
 * sent and stops the instant the result is in hand. This is the "transport
 * layer" measurement (finding #3a). The separate agent-layer measurement
 * (token cost of a real Claude Code session) lives in agent.ts.
 */

import { randomUUID } from "node:crypto";
import { watch } from "node:fs";
import { readFile, rename, unlink, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import type { BridgeJob } from "./scenarios.js";

/**
 * Reads turbofig's pairing token, trimmed, from `TURBOFIG_BRIDGE_DIR/token`
 * or `~/.turbofig/token`. The daemon now requires `Authorization: Bearer
 * <token>` on `/job` and `/mcp` (another local macOS account can otherwise
 * reach 127.0.0.1 and drive the Figma plugin), so the turbofig MCP transport
 * below must read and send it.
 */
async function readTurbofigToken(): Promise<string> {
  const dir = process.env.TURBOFIG_BRIDGE_DIR ?? join(homedir(), ".turbofig");
  const raw = await readFile(join(dir, "token"), "utf8");
  return raw.trim();
}

/** One job's measured outcome. Bytes are exact wire sizes, never estimated. */
export interface SubmitResult {
  result: unknown;
  wallMs: number;
  requestBytes: number;
  responseBytes: number;
  ok: boolean;
}

/** A transport submits one job and reports its measured outcome. */
export interface Transport {
  submit(job: BridgeJob): Promise<SubmitResult>;
  /** Optional one-time setup (MCP handshake). Call before the first submit. */
  init?(): Promise<void>;
  /** Optional one-time teardown (close sockets/watchers). */
  close?(): Promise<void>;
}

// ---------------------------------------------------------------------------
// File-bridge transport (turbofig-bridge)
// ---------------------------------------------------------------------------

/** Map a BridgeJob onto the daemon's bridge wire shape (omits unset fields). */
function toBridgePayload(job: BridgeJob): Record<string, unknown> {
  const payload: Record<string, unknown> = { op: job.op };
  if (job.code !== undefined) payload.code = job.code;
  if (job.fileKey !== undefined) payload.fileKey = job.fileKey;
  if (job.fields !== undefined) payload.fields = job.fields;
  if (job.depth !== undefined) payload.depth = job.depth;
  if (job.nodeId !== undefined) payload.nodeId = job.nodeId;
  if (job.scale !== undefined) payload.scale = job.scale;
  if (job.returnMode !== undefined) payload.return = job.returnMode;
  if (job.maxDim !== undefined) payload.maxDim = job.maxDim;
  if (job.fullRes !== undefined) payload.fullRes = job.fullRes;
  return payload;
}

/** The daemon always replies with an object carrying "ok". Default true for
 * a reply that somehow omits it, rather than silently failing every run. */
function isBridgeOk(parsed: unknown): boolean {
  if (parsed && typeof parsed === "object" && "ok" in parsed) {
    return (parsed as { ok: unknown }).ok === true;
  }
  return true;
}

/**
 * Wait for `<outboxDir>/<id>.json` to appear, using fs.watch for fast
 * notice with a 100ms backstop poll (fs.watch can miss events on some
 * filesystems, and is unsupported on a few platforms). Rejects on timeout.
 */
function waitForOutbox(outboxDir: string, id: string, timeoutMs: number): Promise<string> {
  const target = `${id}.json`;
  return new Promise((resolve, reject) => {
    let settled = false;
    let watcher: ReturnType<typeof watch> | undefined;
    let fallback: ReturnType<typeof setInterval> | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const cleanup = () => {
      watcher?.close();
      if (fallback) clearInterval(fallback);
      if (timer) clearTimeout(timer);
    };

    const tryRead = async () => {
      if (settled) return;
      try {
        const data = await readFile(join(outboxDir, target), "utf8");
        if (settled) return;
        settled = true;
        cleanup();
        resolve(data);
      } catch {
        // Not written yet, or mid-rename. Keep waiting.
      }
    };

    try {
      watcher = watch(outboxDir, (_event, filename) => {
        if (filename === target) void tryRead();
      });
    } catch {
      // fs.watch unsupported here (platform or filesystem); fall back to poll only.
      watcher = undefined;
    }

    fallback = setInterval(() => void tryRead(), 100);
    void tryRead(); // catches a result that landed before the watcher attached

    timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error(`Timeout waiting for bridge result: job ${id}`));
    }, timeoutMs);
  });
}

/**
 * Real file-bridge transport.
 * Writes each job atomically (write <id>.json.tmp, then rename) so the
 * daemon never sees a partial job file. Deletes inbox/outbox files for the
 * job in a finally block so no temp files accumulate.
 */
export function fileBridgeTransport(bridgeDir: string, timeoutMs: number): Transport {
  const inboxDir = join(bridgeDir, "inbox");
  const outboxDir = join(bridgeDir, "outbox");

  return {
    async submit(job: BridgeJob): Promise<SubmitResult> {
      const id = randomUUID();
      const inboxTmp = join(inboxDir, `${id}.json.tmp`);
      const inboxPath = join(inboxDir, `${id}.json`);
      const outboxPath = join(outboxDir, `${id}.json`);
      const payload = JSON.stringify(toBridgePayload(job));
      const requestBytes = Buffer.byteLength(payload);

      const start = Date.now();
      try {
        await writeFile(inboxTmp, payload);
        await rename(inboxTmp, inboxPath);
        const text = await waitForOutbox(outboxDir, id, timeoutMs);
        const wallMs = Date.now() - start;
        const responseBytes = Buffer.byteLength(text);
        try {
          const parsed = JSON.parse(text);
          return { result: parsed, wallMs, requestBytes, responseBytes, ok: isBridgeOk(parsed) };
        } catch {
          return { result: text, wallMs, requestBytes, responseBytes, ok: false };
        }
      } finally {
        await unlink(inboxPath).catch(() => {});
        await unlink(inboxTmp).catch(() => {});
        await unlink(outboxPath).catch(() => {});
      }
    },
  };
}

// ---------------------------------------------------------------------------
// MCP HTTP transport (turbofig-mcp, console-mcp)
// ---------------------------------------------------------------------------

/** Maps a BridgeJob to a target's native tool name and argument shape, and
 * decides whether a reply counts as success for that target's reply shape. */
export interface McpAdapter {
  toolCall(job: BridgeJob): { name: string; arguments: Record<string, unknown> };
  isOk(parsedContent: unknown, isError: boolean): boolean;
}

function dropUndefined(obj: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj)) {
    if (v !== undefined) out[k] = v;
  }
  return out;
}

function defaultIsOk(parsedContent: unknown, isError: boolean): boolean {
  if (isError) return false;
  if (parsedContent && typeof parsedContent === "object") {
    if ("ok" in parsedContent) return (parsedContent as { ok: unknown }).ok === true;
    if ("error" in parsedContent) {
      const err = (parsedContent as { error: unknown }).error;
      return err === undefined || err === null;
    }
  }
  return true;
}

/** turbofig's four tools: turbofig_execute, turbofig_get_selection, turbofig_screenshot, turbofig_status. */
export const turbofigMcpAdapter: McpAdapter = {
  toolCall(job) {
    switch (job.op) {
      case "execute":
        return {
          name: "turbofig_execute",
          arguments: dropUndefined({ code: job.code, fileKey: job.fileKey }),
        };
      case "get_selection":
        return {
          name: "turbofig_get_selection",
          arguments: dropUndefined({ fileKey: job.fileKey, fields: job.fields, depth: job.depth }),
        };
      case "screenshot":
        return {
          name: "turbofig_screenshot",
          arguments: dropUndefined({
            scale: job.scale,
            nodeId: job.nodeId,
            return: job.returnMode,
            fileKey: job.fileKey,
            maxDim: job.maxDim,
            fullRes: job.fullRes,
          }),
        };
      case "status":
        return { name: "turbofig_status", arguments: dropUndefined({ fileKey: job.fileKey }) };
    }
  },
  isOk: defaultIsOk,
};

/** figma-console-mcp's equivalent tools: figma_execute, figma_get_selection, figma_take_screenshot.
 * No status-equivalent tool and no multi-file fileKey; both are dropped. */
export const consoleMcpAdapter: McpAdapter = {
  toolCall(job) {
    switch (job.op) {
      case "execute":
        return { name: "figma_execute", arguments: { code: job.code } };
      case "get_selection":
        // console-mcp has no fields/depth shaping: figma_get_selection's
        // only knob is verbose, which (see figma-console-mcp
        // dist/local.js) fetches fills, strokes, effects, and other extra
        // node props in one extra round trip. The turbofig job in
        // scenarios.ts asks for fields: ["fills"], so verbose: true is the
        // closest match available; it also returns strokes/effects/etc
        // that turbofig's narrower request does not, so the byte counts are
        // not exactly equal. See bench/README.md "Known limits".
        return { name: "figma_get_selection", arguments: { verbose: true } };
      case "screenshot":
        return {
          name: "figma_take_screenshot",
          arguments: dropUndefined({ nodeId: job.nodeId, format: "png", scale: job.scale ?? 2 }),
        };
      case "status":
        throw new Error(
          "console-mcp has no status-equivalent tool; omit status jobs for this target",
        );
    }
  },
  isOk: defaultIsOk,
};

/** Pull the final `data:` line's JSON-RPC envelope out of an SSE response body. */
function parseSse(text: string): { result?: unknown; error?: unknown } {
  const lines = text.split("\n").filter((l) => l.startsWith("data:"));
  if (lines.length === 0) {
    throw new Error(`Expected an SSE response, got: ${text.slice(0, 200)}`);
  }
  const last = lines[lines.length - 1].slice("data:".length).trim();
  return JSON.parse(last);
}

/**
 * MCP streamable-http transport. Reproduces the handshake in scripts/handshake.sh:
 * POST initialize, capture mcp-session-id from response headers, POST
 * notifications/initialized, then POST tools/call per job, reusing the
 * session id. All requests carry Accept: application/json, text/event-stream.
 */
export function mcpTransport(baseUrl: string, adapter: McpAdapter, timeoutMs: number): Transport {
  let sessionId: string | null = null;
  let nextId = 1;
  let tokenPromise: Promise<string> | null = null;
  const accept = "application/json, text/event-stream";

  async function rpc(body: Record<string, unknown>): Promise<{ text: string; headers: Headers }> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        Accept: accept,
      };
      if (sessionId) headers["mcp-session-id"] = sessionId;
      // Only turbofig's own endpoint needs the pairing token; console-mcp
      // has no such gate. A missing/unreadable token file is left to fail
      // naturally at the daemon (401), not swallowed silently here.
      if (adapter === turbofigMcpAdapter) {
        if (!tokenPromise) tokenPromise = readTurbofigToken();
        headers.Authorization = `Bearer ${await tokenPromise}`;
      }
      const res = await fetch(baseUrl, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      const text = await res.text();
      return { text, headers: res.headers };
    } finally {
      clearTimeout(timer);
    }
  }

  async function ensureSession(): Promise<void> {
    if (sessionId) return;
    const { headers } = await rpc({
      jsonrpc: "2.0",
      id: nextId++,
      method: "initialize",
      params: {
        protocolVersion: "2025-03-26",
        clientInfo: { name: "turbofig-bench", version: "0.1.0" },
        capabilities: {},
      },
    });
    const sid = headers.get("mcp-session-id");
    if (!sid) {
      throw new Error(`initialize did not return an mcp-session-id header (${baseUrl})`);
    }
    sessionId = sid;
    await rpc({ jsonrpc: "2.0", method: "notifications/initialized", params: {} });
  }

  return {
    async init() {
      await ensureSession();
    },
    async submit(job: BridgeJob): Promise<SubmitResult> {
      await ensureSession();
      const { name, arguments: args } = adapter.toolCall(job);
      const requestBody = {
        jsonrpc: "2.0",
        id: nextId++,
        method: "tools/call",
        params: { name, arguments: args },
      };
      const requestBytes = Buffer.byteLength(JSON.stringify(requestBody));

      const start = Date.now();
      const { text } = await rpc(requestBody);
      const wallMs = Date.now() - start;
      const responseBytes = Buffer.byteLength(text);

      const envelope = parseSse(text) as {
        result?: { content?: Array<{ type: string; text?: string }>; isError?: boolean };
        error?: { message?: string };
      };
      if (envelope.error) {
        return { result: envelope.error, wallMs, requestBytes, responseBytes, ok: false };
      }
      const isError = envelope.result?.isError === true;
      const block = envelope.result?.content?.[0];
      let parsedContent: unknown = block?.text;
      if (typeof block?.text === "string") {
        try {
          parsedContent = JSON.parse(block.text);
        } catch {
          parsedContent = block.text;
        }
      }
      return {
        result: parsedContent,
        wallMs,
        requestBytes,
        responseBytes,
        ok: adapter.isOk(parsedContent, isError),
      };
    },
  };
}

// ---------------------------------------------------------------------------
// Dry-run stub
// ---------------------------------------------------------------------------

/** Stub transport for dry-run mode. No daemon, no network, no Figma file.
 * Reports real byte counts for the canned request/response shape so --dry-run
 * stays useful for sanity-checking job payload sizes. */
export function stubTransport(cannedResult: unknown = { ok: true }): Transport {
  return {
    async submit(job: BridgeJob): Promise<SubmitResult> {
      const requestBytes = Buffer.byteLength(JSON.stringify(toBridgePayload(job)));
      const responseBytes = Buffer.byteLength(JSON.stringify(cannedResult));
      return { result: cannedResult, wallMs: 0, requestBytes, responseBytes, ok: true };
    },
  };
}
