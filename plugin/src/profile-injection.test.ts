/**
 * Injection tests for the built-in taste profiles.
 *
 * The daemon prepends a profile JS script to the user code, then the plugin
 * wraps that whole string with the deprecation preamble inside an async
 * function. These tests build the SAME combined string and actually RUN it, so
 * a profile that is syntactically broken or that throws under strict mode fails
 * here. A substring match alone would not catch that.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { wrapUserCode } from "./protocol";

/** The AsyncFunction constructor, the same one the plugin uses to run evals. */
const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
  ...args: string[]
) => (...args: unknown[]) => Promise<unknown>;

/** Read a built-in profile source by id. */
function profileSrc(id: string): string {
  return readFileSync(join(import.meta.dir, `../../skills/profiles/${id}.js`), "utf8");
}

/**
 * Run the combined injected eval exactly as the plugin does and return the
 * value the user code produced. The user code reads the profile through tf.taste.
 */
async function runInjected(id: string, userCode: string): Promise<unknown> {
  const combined = `tf.taste = (() => {\n${profileSrc(id)}\nreturn taste;\n})();\n${userCode}`;
  const fn = new AsyncFunction("figma", "tf", wrapUserCode(combined));
  return await fn(undefined, {});
}

describe("profile injection reaches user code", () => {
  for (const id of ["impeccable", "editorial", "minimal"]) {
    test(`${id}: injected taste is in scope and carries the right id`, async () => {
      // User code reads the injected profile through tf.taste and returns its id.
      const result = await runInjected(id, "return tf.taste.id;");
      expect(result).toBe(id);
    });

    test(`${id}: injected taste exposes the constraint keys`, async () => {
      const result = (await runInjected(
        id,
        "return { hasSpacing: Array.isArray(tf.taste.spacing), hasBlocklist: Array.isArray(tf.taste.blocklist) };",
      )) as { hasSpacing: boolean; hasBlocklist: boolean };
      expect(result.hasSpacing).toBe(true);
      expect(result.hasBlocklist).toBe(true);
    });
  }

  test("collision: user code that declares const taste does not shadow tf.taste", async () => {
    // Proves that a top-level const taste in user code does not collide with the
    // profile assignment, because the profile lives inside an IIFE.
    const result = await runInjected("impeccable", "const taste = 123; return tf.taste.id;");
    expect(result).toBe("impeccable");
  });

  test("isolation: sequential invocations do not bleed profile state", async () => {
    // Each call to runInjected passes a fresh tf object. The profile must not
    // persist from one invocation to the next through shared state.
    const first = await runInjected("editorial", "return tf.taste.id;");
    const second = await runInjected("minimal", "return tf.taste.id;");
    expect(first).toBe("editorial");
    expect(second).toBe("minimal");
  });
});
