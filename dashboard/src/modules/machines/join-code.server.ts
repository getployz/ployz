import { createHash } from "node:crypto";

const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";

/**
 * The code `ployz device approve --join` takes for a joining Server: the first 100 bits of a SHA-256 over its Machine ID
 * and WireGuard public key, as 20 base32 digits. It matches `ployz_core::authority::join::code`.
 */
export function joinCode(machineId: string, publicKey: readonly number[]): string {
  const digest = createHash("sha256")
    .update("ployz-join-v1\n")
    .update(machineId, "utf8")
    .update(Uint8Array.from(publicKey))
    .digest();
  const bits = BigInt(`0x${digest.subarray(0, 16).toString("hex")}`);
  let code = "";
  for (let index = 1n; index <= 20n; index += 1n) {
    code += ALPHABET[Number((bits >> (128n - 5n * index)) & 31n)];
  }
  return code;
}
