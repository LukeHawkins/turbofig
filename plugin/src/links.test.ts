import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createDispatcher } from "./code";
import { ALLOWED_EXTERNAL_URLS, isAllowedExternalUrl, isInboundMessage } from "./protocol";
import { buildOpenUrlMessage } from "./ui/ui-logic";

const template = readFileSync(join(import.meta.dir, "ui/template.html"), "utf8");
const mainTs = readFileSync(join(import.meta.dir, "ui/main.ts"), "utf8");

describe("external link allowlist", () => {
  test("accepts exactly the 2 allowed URLs", () => {
    expect(ALLOWED_EXTERNAL_URLS).toEqual([
      "https://lukehawkins.eu",
      "https://github.com/LukeHawkins/turbofig",
    ]);
    for (const url of ALLOWED_EXTERNAL_URLS) expect(isAllowedExternalUrl(url)).toBe(true);
  });

  test("rejects every other URL", () => {
    for (const url of [
      "https://evil.example",
      "http://lukehawkins.eu",
      "https://lukehawkins.eu/",
      "https://lukehawkins.eu.evil.example",
      "https://github.com/LukeHawkins/other",
      "javascript:alert(1)",
      "",
      42,
      null,
    ]) {
      expect(isAllowedExternalUrl(url)).toBe(false);
    }
  });

  test("OPEN_URL is an inbound message only with a string url", () => {
    expect(isInboundMessage({ type: "OPEN_URL", url: "https://lukehawkins.eu" })).toBe(true);
    expect(isInboundMessage({ type: "OPEN_URL" })).toBe(false);
  });
});

describe("code.ts OPEN_URL", () => {
  function run(url: string): string[] {
    const opened: string[] = [];
    const figma = { openExternal: (u: string) => opened.push(u) } as unknown as PluginAPI;
    createDispatcher(figma, () => {})({ type: "OPEN_URL", url });
    return opened;
  }

  test("calls figma.openExternal for an allowed URL", () => {
    expect(run("https://github.com/LukeHawkins/turbofig")).toEqual([
      "https://github.com/LukeHawkins/turbofig",
    ]);
  });

  test("does not call figma.openExternal for another URL", () => {
    expect(run("https://evil.example")).toEqual([]);
  });
});

describe("UI links", () => {
  test("each external link posts OPEN_URL with an allowed URL", () => {
    const urls = [...template.matchAll(/data-open-url="([^"]+)"/g)].map((m) => m[1]);
    expect(urls.sort()).toEqual([...ALLOWED_EXTERNAL_URLS].sort());
    for (const url of urls) {
      expect(buildOpenUrlMessage(url)).toEqual({ type: "OPEN_URL", url });
    }
    expect(mainTs).toContain("[data-open-url]");
    expect(mainTs).toContain("buildOpenUrlMessage");
  });

  test("no plain external anchors or window.open remain", () => {
    expect(template).not.toMatch(/<a\s[^>]*href=/);
    expect(template).not.toContain("window.open");
  });
});

describe("version and activity markup", () => {
  test("version is read from package.json at build time and shown in the footer", () => {
    const build = readFileSync(join(import.meta.dir, "../build-ui.ts"), "utf8");
    expect(build).toContain('join(scriptDir, "package.json")');
    expect(build).toContain("__PLUGIN_VERSION__: JSON.stringify(pluginVersion)");
    const header = template.slice(
      template.indexOf('<header class="panel-header">'),
      template.indexOf("</header>"),
    );
    expect(header).not.toContain("plugin-version");
    const footer = template.slice(template.indexOf('<div class="panel-footer">'));
    expect(footer.indexOf('id="plugin-version"')).toBeGreaterThan(footer.indexOf('id="nav-more"'));
  });

  test("activity squares sit in the header with a reduced-motion rule", () => {
    expect(template).toContain("ACTIVITY SQUARES");
    expect(template).toContain('id="activity"');
    expect(template).toContain("prefers-reduced-motion");
  });
});
