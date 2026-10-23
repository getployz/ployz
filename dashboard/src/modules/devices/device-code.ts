import { createHash } from "node:crypto";

const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";

export const DEVICE_KEY = /^[0-9a-f]{64}$/;

/**
 * The code a Device's holder reads out for `key`: the top 50 bits of its SHA-256 in lowercase base32, as the Engine's
 * `DeviceCode::of` computes it.
 */
export function deviceCodeOf(key: string) {
  const bits = createHash("sha256").update(Buffer.from(key, "hex")).digest().readBigUInt64BE(0);
  return Array.from({ length: 10 }, (_, index) => BASE32[Number((bits >> BigInt(59 - 5 * index)) & 31n)]).join("");
}
