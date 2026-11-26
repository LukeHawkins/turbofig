import { describe, expect, test } from "bun:test";
import { createDispatcher } from "../code";
import { isAllowedExternalUrl, isInboundMessage, parseReleaseVersion } from "../protocol";
import {
  daemonMessageAction,
  isUiOnlyControlType,
  mainMessageAction,
  screenSize,
  UPDATE_BANNER_HEIGHT,
  UPDATE_HEIGHT,
} from "./ui-logic";
import {
  type FetchLike,
  fetchLatestVersion,
  isConnectionError,
  isNewerVersion,
  LATEST_RELEASE_API_URL,
  runUpdateCheck,
  shouldCheckForUpdate,
  UPDATE_CHECK_INTERVAL_MS,
  updateBannerVersion,
  versionFromRelease,
} from "./update-check";

const DAY = UPDATE_CHECK_INTERVAL_MS;
const NOW = 1_800_000_000_000;

describe("check scheduling", () => {
  test("runs when never checked", () => {
    expect(shouldCheckForUpdate(true, null, NOW)).toBe(true);
  });
  test("waits until 24 h have passed", () => {
    expect(shouldCheckForUpdate(true, NOW - DAY + 1, NOW)).toBe(false);
    expect(shouldCheckForUpdate(true, NOW - DAY, NOW)).toBe(true);
    expect(shouldCheckForUpdate(true, NOW - 1000, NOW)).toBe(false);
  });
  test("runs when the stored time is in the future (clock moved back)", () => {
    expect(shouldCheckForUpdate(true, NOW + 5000, NOW)).toBe(true);
  });
  test("never runs when the setting is off", () => {
    expect(shouldCheckForUpdate(false, null, NOW)).toBe(false);
    expect(shouldCheckForUpdate(false, NOW - 10 * DAY, NOW)).toBe(false);
  });
  test("runUpdateCheck does not fetch when off or not due", async () => {
    let calls = 0;
    const fetchLatest = async () => {
      calls++;
      return "1.0.0";
    };
    const off = await runUpdateCheck({ enabled: false, lastCheckAt: null, now: NOW, fetchLatest });
    const fresh = await runUpdateCheck({
      enabled: true,
      lastCheckAt: NOW - 1000,
      now: NOW,
      fetchLatest,
    });
    expect(calls).toBe(0);
    expect(off.ran).toBe(false);
    expect(fresh).toEqual({ ran: false, lastCheckAt: NOW - 1000, latestVersion: null });
  });
  test("runUpdateCheck fetches when due and stamps the time", async () => {
    const out = await runUpdateCheck({
      enabled: true,
      lastCheckAt: null,
      now: NOW,
      fetchLatest: async () => "0.3.0",
    });
    expect(out).toEqual({ ran: true, lastCheckAt: NOW, latestVersion: "0.3.0" });
  });
  test("a failed check still stamps the time, so it retries the next day", async () => {
    const out = await runUpdateCheck({
      enabled: true,
      lastCheckAt: NOW - 2 * DAY,
      now: NOW,
      fetchLatest: async () => null,
    });
    expect(out).toEqual({ ran: true, lastCheckAt: NOW, latestVersion: null });
  });
});

describe("tag parsing", () => {
  test("strips a leading v and accepts x.y.z", () => {
    expect(parseReleaseVersion("v0.2.1")).toBe("0.2.1");
    expect(parseReleaseVersion("0.2.1")).toBe("0.2.1");
    expect(parseReleaseVersion("v10.20.30")).toBe("10.20.30");
  });
  test("rejects anything else", () => {
    for (const bad of [
      "",
      "v",
      "0.2",
      "0.2.1.4",
      "v0.2.1-beta.1",
      "0.2.1+build",
      "vv0.2.1",
      "V0.2.1",
      "latest",
      "0.2.x",
      " 0.2.1",
      "0.2.1\n",
      "<script>",
      1,
      null,
      undefined,
      {},
    ]) {
      expect(parseReleaseVersion(bad)).toBeNull();
    }
  });
  test("versionFromRelease reads tag_name only", () => {
    expect(versionFromRelease({ tag_name: "v0.3.0" })).toBe("0.3.0");
    expect(versionFromRelease({ tag_name: "nightly" })).toBeNull();
    expect(versionFromRelease({ name: "v0.3.0" })).toBeNull();
    expect(versionFromRelease(null)).toBeNull();
    expect(versionFromRelease("v0.3.0")).toBeNull();
  });
});

describe("version compare", () => {
  test("newer, older, equal", () => {
    expect(isNewerVersion("0.3.0", "0.2.0")).toBe(true);
    expect(isNewerVersion("0.2.1", "0.2.0")).toBe(true);
    expect(isNewerVersion("1.0.0", "0.9.9")).toBe(true);
    expect(isNewerVersion("0.10.0", "0.9.0")).toBe(true);
    expect(isNewerVersion("0.1.0", "0.2.0")).toBe(false);
    expect(isNewerVersion("0.2.0", "0.2.0")).toBe(false);
  });
  test("invalid on either side is never newer", () => {
    expect(isNewerVersion("0.3", "0.2.0")).toBe(false);
    expect(isNewerVersion("abc", "0.2.0")).toBe(false);
    expect(isNewerVersion("0.3.0", "dev")).toBe(false);
    expect(isNewerVersion("0.3.0", "")).toBe(false);
  });
});

describe("fetchLatestVersion", () => {
  const ok =
    (body: unknown, status = 200) =>
    async () => ({
      ok: status === 200,
      status,
      json: async () => body,
    });

  test("sends a plain GET to the releases API with only the Accept header", async () => {
    const seen: { url: string; init: Parameters<FetchLike>[1] }[] = [];
    const f: FetchLike = async (url, init) => {
      seen.push({ url, init });
      return { ok: true, status: 200, json: async () => ({ tag_name: "v0.3.0" }) };
    };
    expect(await fetchLatestVersion(f)).toBe("0.3.0");
    expect(seen).toHaveLength(1);
    expect(seen[0]?.url).toBe(LATEST_RELEASE_API_URL);
    expect(LATEST_RELEASE_API_URL).toBe(
      "https://api.github.com/repos/LukeHawkins/turbofig/releases/latest",
    );
    expect(seen[0]?.init.headers).toEqual({ Accept: "application/vnd.github+json" });
    const keys = Object.keys(seen[0]?.init ?? {}).sort();
    expect(keys).toEqual(["headers", "signal"]);
  });
  test("non-200, bad body and thrown errors give null", async () => {
    expect(await fetchLatestVersion(ok({ tag_name: "v1.0.0" }, 403))).toBeNull();
    expect(await fetchLatestVersion(ok({ tag_name: "v1.0.0" }, 404))).toBeNull();
    expect(await fetchLatestVersion(ok({ tag_name: "oops" }))).toBeNull();
    expect(await fetchLatestVersion(ok(null))).toBeNull();
    expect(
      await fetchLatestVersion(async () => {
        throw new Error("offline");
      }),
    ).toBeNull();
    expect(
      await fetchLatestVersion(async () => ({
        ok: true,
        status: 200,
        json: async () => {
          throw new Error("bad json");
        },
      })),
    ).toBeNull();
  });
  test("a request that never answers is aborted at the timeout", async () => {
    const f: FetchLike = (_url, init) =>
      new Promise((_resolve, reject) => {
        init.signal?.addEventListener("abort", () => reject(new Error("aborted")));
      });
    const started = Date.now();
    expect(await fetchLatestVersion(f, 30)).toBeNull();
    expect(Date.now() - started).toBeLessThan(2000);
  });
});

describe("banner", () => {
  const base = {
    enabled: true,
    latestVersion: "0.3.0",
    dismissedVersion: null,
    pluginVersion: "0.2.0",
    staleShowing: false,
    connectionError: false,
  };
  test("shows for a newer version", () => {
    expect(updateBannerVersion(base)).toBe("0.3.0");
  });
  test("hidden for equal, older, invalid or missing versions", () => {
    expect(updateBannerVersion({ ...base, latestVersion: "0.2.0" })).toBeNull();
    expect(updateBannerVersion({ ...base, latestVersion: "0.1.0" })).toBeNull();
    expect(updateBannerVersion({ ...base, latestVersion: "x" })).toBeNull();
    expect(updateBannerVersion({ ...base, latestVersion: null })).toBeNull();
    expect(updateBannerVersion({ ...base, pluginVersion: "dev" })).toBeNull();
  });
  test("hidden when the setting is off", () => {
    expect(updateBannerVersion({ ...base, enabled: false })).toBeNull();
  });
  test("dismiss hides that version and a newer version shows again", () => {
    const dismissed = { ...base, dismissedVersion: "0.3.0" };
    expect(updateBannerVersion(dismissed)).toBeNull();
    expect(updateBannerVersion({ ...dismissed, latestVersion: "0.3.1" })).toBe("0.3.1");
    expect(updateBannerVersion({ ...dismissed, latestVersion: "0.4.0" })).toBe("0.4.0");
    expect(updateBannerVersion({ ...dismissed, dismissedVersion: "0.9.0" })).toBeNull();
  });
  test("the stale banner and a connection error win", () => {
    expect(updateBannerVersion({ ...base, staleShowing: true })).toBeNull();
    expect(updateBannerVersion({ ...base, connectionError: true })).toBeNull();
  });
  test("connection error: offline, sticky status text, or no pairing token", () => {
    expect(isConnectionError("connected", null, true)).toBe(false);
    expect(isConnectionError("connecting", null, true)).toBe(false);
    expect(isConnectionError("reconnecting", null, true)).toBe(false);
    expect(isConnectionError("offline", null, true)).toBe(true);
    expect(isConnectionError("connected", "turbofig was reset", true)).toBe(true);
    expect(isConnectionError("connecting", null, false)).toBe(true);
  });
});

describe("release URLs", () => {
  test("rejects github release and other paths", () => {
    for (const bad of [
      "https://github.com/LukeHawkins/turbofig/releases",
      "https://github.com/LukeHawkins/turbofig/releases/latest",
      "https://github.com/LukeHawkins/turbofig/releases/tag/v0.3.0",
      "https://github.com/LukeHawkins/turbofig/issues",
    ]) {
      expect(isAllowedExternalUrl(bad)).toBe(false);
    }
  });
  test("the main thread refuses a release URL", () => {
    const opened: string[] = [];
    const figma = { openExternal: (u: string) => opened.push(u) } as unknown as PluginAPI;
    const d = createDispatcher(figma, () => {});
    d({ type: "OPEN_URL", url: "https://github.com/LukeHawkins/turbofig/releases/tag/v0.3.0" });
    expect(opened).toEqual([]);
  });
});

describe("UI-only types stay off the relays", () => {
  const types = ["SET_UPDATE_CHECK", "SET_UPDATE_LATEST", "DISMISS_UPDATE"];
  test("main-thread relay drops them", () => {
    for (const t of types) {
      expect(isUiOnlyControlType(t)).toBe(true);
      expect(mainMessageAction(t)).toBe("drop");
    }
  });
  test("daemon relay drops them, and UPDATE_STATE too", () => {
    for (const t of [...types, "UPDATE_STATE"]) expect(daemonMessageAction(t)).toBe("drop");
  });
  test("UPDATE_STATE from the main thread is consumed, never relayed", () => {
    expect(mainMessageAction("UPDATE_STATE")).toBe("updatestate");
  });
});

describe("main thread storage", () => {
  function setup(initial: Record<string, unknown> = {}) {
    const store = new Map<string, unknown>(Object.entries(initial));
    const posts: Record<string, unknown>[] = [];
    const figma = {
      fileKey: "k",
      root: { name: "f" },
      clientStorage: {
        getAsync: async (k: string) => store.get(k),
        setAsync: async (k: string, v: unknown) => {
          store.set(k, v);
        },
      },
    } as unknown as PluginAPI;
    const d = createDispatcher(figma, (m) => posts.push(m as Record<string, unknown>));
    return { store, posts, d };
  }
  const tick = () => new Promise((r) => setTimeout(r, 5));

  test("READY sends the defaults: on, nothing stored", async () => {
    const { posts, d } = setup();
    d({ type: "READY" });
    await tick();
    expect(posts.find((p) => p.type === "UPDATE_STATE")).toEqual({
      type: "UPDATE_STATE",
      enabled: true,
      lastCheckAt: null,
      latestVersion: null,
      dismissedVersion: null,
    });
  });
  test("settings, check result and dismissal persist and come back on READY", async () => {
    const a = setup();
    a.d({ type: "SET_UPDATE_CHECK", enabled: false });
    a.d({ type: "SET_UPDATE_LATEST", lastCheckAt: NOW, latestVersion: "0.3.0" });
    a.d({ type: "DISMISS_UPDATE", version: "0.3.0" });
    await tick();
    const b = setup(Object.fromEntries(a.store));
    b.d({ type: "READY" });
    await tick();
    expect(b.posts.find((p) => p.type === "UPDATE_STATE")).toEqual({
      type: "UPDATE_STATE",
      enabled: false,
      lastCheckAt: NOW,
      latestVersion: "0.3.0",
      dismissedVersion: "0.3.0",
    });
  });
  test("a failed check keeps the stored version", async () => {
    const { store, d } = setup({ "turbofig:updateLatestVersion": "0.3.0" });
    d({ type: "SET_UPDATE_LATEST", lastCheckAt: NOW });
    await tick();
    expect(store.get("turbofig:updateLatestVersion")).toBe("0.3.0");
    expect(store.get("turbofig:updateLastCheckAt")).toBe(NOW);
  });
  test("a newer release after a dismissal shows the banner again", async () => {
    const a = setup();
    a.d({ type: "DISMISS_UPDATE", version: "0.3.0" });
    a.d({ type: "SET_UPDATE_LATEST", lastCheckAt: NOW, latestVersion: "0.3.1" });
    await tick();
    const b = setup(Object.fromEntries(a.store));
    b.d({ type: "READY" });
    await tick();
    const s = b.posts.find((p) => p.type === "UPDATE_STATE") as {
      latestVersion: string;
      dismissedVersion: string;
    };
    expect(
      updateBannerVersion({
        enabled: true,
        latestVersion: s.latestVersion,
        dismissedVersion: s.dismissedVersion,
        pluginVersion: "0.2.0",
        staleShowing: false,
        connectionError: false,
      }),
    ).toBe("0.3.1");
  });
  test("bad stored or posted values are refused", async () => {
    const { posts, d } = setup({
      "turbofig:updateLatestVersion": "<script>",
      "turbofig:updateLastCheckAt": "yesterday",
    });
    d({ type: "READY" });
    await tick();
    const s = posts.find((p) => p.type === "UPDATE_STATE");
    expect(s?.latestVersion).toBeNull();
    expect(s?.lastCheckAt).toBeNull();
    expect(isInboundMessage({ type: "DISMISS_UPDATE", version: "nope" })).toBe(false);
    expect(
      isInboundMessage({ type: "SET_UPDATE_LATEST", lastCheckAt: 1, latestVersion: "x" }),
    ).toBe(false);
    expect(isInboundMessage({ type: "SET_UPDATE_CHECK", enabled: "yes" })).toBe(false);
  });
});

describe("panel size", () => {
  test("the banner adds its height to the main screen only", () => {
    const plain = screenSize("main").height;
    expect(screenSize("main", true, true, true).height).toBe(plain + UPDATE_BANNER_HEIGHT);
    expect(screenSize("about", true, true, true)).toEqual(screenSize("about"));
  });
  test("the update screen has its own height", () => {
    expect(screenSize("update")).toEqual({ width: 300, height: UPDATE_HEIGHT });
  });
});
