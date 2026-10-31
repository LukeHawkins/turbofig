import { chmodSync, writeFileSync } from "node:fs";

/**
 * Write a file that only its owner can read (mode 0600). Build outputs can
 * embed the real pairing token, so another account on the Mac must not read
 * them. `mode` applies only when the file is created, so chmod it as well.
 */
export function writeOwnerOnly(path: string, text: string): void {
  writeFileSync(path, text, { mode: 0o600 });
  chmodSync(path, 0o600);
}
