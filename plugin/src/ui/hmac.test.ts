import { describe, expect, test } from "bun:test";
import { constantTimeEqual, hmacSha256, hmacSha256Hex, sha256, toHex, utf8Bytes } from "./hmac";

const TOKEN = "a".repeat(64);
const NONCE = "00112233445566778899aabbccddeeff";

describe("sha256", () => {
  test("empty input", () => {
    expect(toHex(sha256(new Uint8Array()))).toBe(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
  });

  test("abc", () => {
    expect(toHex(sha256(utf8Bytes("abc")))).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });

  test("multi-block input matches node crypto", () => {
    const { createHash } = require("node:crypto");
    for (const len of [55, 56, 63, 64, 65, 119, 120, 1000]) {
      const data = new Uint8Array(len).map((_, i) => i % 251);
      expect(toHex(sha256(data))).toBe(createHash("sha256").update(data).digest("hex"));
    }
  });
});

describe("hmacSha256", () => {
  test("RFC 4231 case 2", () => {
    expect(hmacSha256Hex("Jefe", "what do ya want for nothing?")).toBe(
      "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    );
  });

  test("RFC 4231 case 6 (key longer than the block size)", () => {
    const key = new Uint8Array(131).fill(0xaa);
    const msg = utf8Bytes("Test Using Larger Than Block-Size Key - Hash Key First");
    expect(toHex(hmacSha256(key, msg))).toBe(
      "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
    );
  });
});

describe("shared cross-check vectors with the Rust daemon", () => {
  test("http-daemon", () => {
    expect(hmacSha256Hex(TOKEN, `turbofig-http-daemon:${NONCE}`)).toBe(
      "aa58167f217dd78d0b1c2056735f63935ce9c9249175603ce936f19340be7eaa",
    );
  });

  test("ws-daemon", () => {
    expect(hmacSha256Hex(TOKEN, `turbofig-ws-daemon:${NONCE}`)).toBe(
      "d70d29b0eb2b2eb28afaed71877cec191003d9b684d8fa66bd471581872e79fe",
    );
  });

  test("ws-plugin", () => {
    expect(hmacSha256Hex(TOKEN, `turbofig-ws-plugin:${NONCE}`)).toBe(
      "ccee347e4ca98ab0341bfb3b391044a3c10cb9fd3b7a0763709abab5042e6fa3",
    );
  });
});

describe("utf8Bytes and constantTimeEqual", () => {
  test("utf8Bytes matches TextEncoder for multi-byte text", () => {
    const s = "aé€\u{1F600}";
    expect(Array.from(utf8Bytes(s))).toEqual(Array.from(new TextEncoder().encode(s)));
  });

  test("constantTimeEqual compares equal, different and unequal-length strings", () => {
    expect(constantTimeEqual("abc", "abc")).toBe(true);
    expect(constantTimeEqual("abc", "abd")).toBe(false);
    expect(constantTimeEqual("abc", "ab")).toBe(false);
  });
});
