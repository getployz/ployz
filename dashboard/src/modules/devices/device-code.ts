import { createHash } from "node:crypto";

const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";

export const DEVICE_KEY = /^[0-9a-f]{64}$/;

/**
 * The name Cloud files a request for `key` under: the top 50 bits of its SHA-256 in lowercase base32, as the Engine's
 * `DeviceCode::of` computes it. The CLI checks the key against the 100-bit code its holder printed, never this.
 */
export function deviceCodeOf(key: string) {
  return codeOfKeyBytes(Buffer.from(key, "hex"));
}

/** The code of a key written as `bytes`, as the Engine's `DeviceCode::hashing` computes it. Passkeys use it too. */
export function codeOfKeyBytes(bytes: Uint8Array) {
  const bits = createHash("sha256").update(bytes).digest().readBigUInt64BE(0);
  return Array.from({ length: 10 }, (_, index) => BASE32[Number((bits >> BigInt(59 - 5 * index)) & 31n)]).join("");
}
