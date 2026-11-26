/**
 * Pure logic for the "newer turbofig" notice. No DOM, no storage.
 * The plugin UI asks GitHub once a day for the latest release version.
 * The daemon makes no network call: this check runs in the plugin only.
 */

import { parseReleaseVersion } from "../protocol";

/** The one URL the check calls: a plain GET, no user or file data. */
export const LATEST_RELEASE_API_URL =
  "https://api.github.com/repos/LukeHawkins/turbofig/releases/latest";

/** The check runs at most once in this time (24 h, in ms). */
export const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;

/** The check gives up after this time (ms). */
export const UPDATE_FETCH_TIMEOUT_MS = 5000;

/** The command the Update page shows. */
export const UPGRADE_COMMAND = "brew upgrade turbofig";

/** Release page of one version, opened from the Update page. */
export function releaseUrl(version: string): string {
  return `https://github.com/LukeHawkins/turbofig/releases/tag/v${version}`;
}

function versionParts(v: string): [number, number, number] | null {
  const bare = parseReleaseVersion(v);
  if (bare === null) return null;
  const [a, b, c] = bare.split(".").map(Number);
  return [a as number, b as number, c as number];
}

/**
 * True only when `candidate` is a valid x.y.z version greater than `current`.
 * An invalid candidate or an invalid current version (for example "dev") is
 * never newer, so no notice shows from bad data.
 */
export function isNewerVersion(candidate: string, current: string): boolean {
  const c = versionParts(candidate);
  const p = versionParts(current);
  if (!c || !p) return false;
  for (let i = 0; i < 3; i++) {
    if (c[i] !== p[i]) return (c[i] as number) > (p[i] as number);
  }
  return false;
}

/**
 * True when a check is due: the setting is on and the last check is missing,
 * 24 h old or more, or in the future (a clock that moved back).
 */
export function shouldCheckForUpdate(
  enabled: boolean,
  lastCheckAt: number | null,
  now: number,
): boolean {
  if (!enabled) return false;
  if (typeof lastCheckAt !== "number" || !Number.isFinite(lastCheckAt)) return true;
  if (lastCheckAt > now) return true;
  return now - lastCheckAt >= UPDATE_CHECK_INTERVAL_MS;
}

/** Reads the version from a GitHub "latest release" body, or null when it is not valid. */
export function versionFromRelease(body: unknown): string | null {
  if (typeof body !== "object" || body === null) return null;
  return parseReleaseVersion((body as { tag_name?: unknown }).tag_name);
}

/** The slice of fetch the check needs. Injected so tests make no network call. */
export type FetchLike = (
  url: string,
  init: { headers: Record<string, string>; signal?: AbortSignal },
) => Promise<{ ok: boolean; status: number; json(): Promise<unknown> }>;

/**
 * Asks GitHub for the latest release version. Plain GET with one Accept
 * header. Returns the x.y.z version, or null on any error, timeout, non-200
 * status or invalid tag. Never throws.
 */
export async function fetchLatestVersion(
  fetchFn: FetchLike,
  timeoutMs = UPDATE_FETCH_TIMEOUT_MS,
): Promise<string | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetchFn(LATEST_RELEASE_API_URL, {
      headers: { Accept: "application/vnd.github+json" },
      signal: controller.signal,
    });
    if (res.status !== 200) return null;
    return versionFromRelease(await res.json());
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

/** What a check run decided. `latestVersion` is null when the check failed or did not run. */
export interface UpdateCheckOutcome {
  ran: boolean;
  lastCheckAt: number | null;
  latestVersion: string | null;
}

/**
 * Runs the check when it is due. When it runs, `lastCheckAt` is `now` even
 * if the fetch failed, so a failure waits a day before the next try.
 * Does nothing (ran false) when the setting is off or the check is not due.
 */
export async function runUpdateCheck(input: {
  enabled: boolean;
  lastCheckAt: number | null;
  now: number;
  fetchLatest: () => Promise<string | null>;
}): Promise<UpdateCheckOutcome> {
  if (!shouldCheckForUpdate(input.enabled, input.lastCheckAt, input.now)) {
    return { ran: false, lastCheckAt: input.lastCheckAt, latestVersion: null };
  }
  const latestVersion = await input.fetchLatest();
  return { ran: true, lastCheckAt: input.now, latestVersion };
}

/** True when a connection error shows in the status line. */
export function isConnectionError(
  state: string,
  override: string | null,
  hasToken: boolean,
): boolean {
  return !hasToken || override !== null || state === "offline";
}

/** Inputs of the banner decision. */
export interface UpdateBannerInput {
  enabled: boolean;
  latestVersion: string | null;
  dismissedVersion: string | null;
  pluginVersion: string;
  /** The "turbofig was updated" banner is showing. */
  staleShowing: boolean;
  /** A connection error is showing. */
  connectionError: boolean;
}

/**
 * Returns the version the banner announces, or null when it stays hidden.
 * Hidden when the setting is off, when the stale banner or a connection
 * error shows (those win), when the version is not newer than the plugin,
 * or when it is not newer than the version the user dismissed.
 */
export function updateBannerVersion(i: UpdateBannerInput): string | null {
  if (!i.enabled || i.staleShowing || i.connectionError) return null;
  if (i.latestVersion === null || !isNewerVersion(i.latestVersion, i.pluginVersion)) return null;
  if (i.dismissedVersion !== null && !isNewerVersion(i.latestVersion, i.dismissedVersion)) {
    return null;
  }
  return i.latestVersion;
}
