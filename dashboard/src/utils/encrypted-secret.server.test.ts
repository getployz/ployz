import { describe, expect, it } from "vitest";
import { decryptSecretValue } from "#/utils/encrypted-secret.server";

const encryptionSecret = "test-encryption-secret";

// Sealed by the Rust Config Store (`ployz-store`'s sealing) with the same secret; its
// tests open vectors sealed here, so both directions stay byte-compatible.
describe("Config Store sealing vectors", () => {
  const vectors = [
    {
      value: "postgres://u:p@db/app",
      sealed: { version: 1 as const, iv: "SrgYXlvf+kiDClRp", tag: "lWhpMscyOmjBR3lWsTdfyg==", ciphertext: "e8ozWcEBW7AAMvtfIL+n0MrB9P6d" },
    },
    {
      value: "pässwörd 🔑 秘密",
      sealed: { version: 1 as const, iv: "Dxo7hNTLCyY8lAST", tag: "qHvkFFAxAcX4Zh12J1hhGQ==", ciphertext: "zRX1ENhMjwNnaaog8fSQqeuWu85ISw==" },
    },
  ];

  it("opens what Rust sealed", () => {
    for (const { value, sealed } of vectors) {
      expect(decryptSecretValue(encryptionSecret, sealed)).toBe(value);
    }
  });

  it("rejects a corrupt tag", () => {
    for (const { sealed } of vectors) {
      expect(() => decryptSecretValue(encryptionSecret, { ...sealed, tag: Buffer.alloc(16).toString("base64") })).toThrow();
    }
  });
});
