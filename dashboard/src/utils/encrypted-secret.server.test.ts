import { describe, expect, it } from "vitest";
import {
  decryptSecretValue,
  encryptSecretValue,
  getPlainVariableValueFingerprint,
  getSealedVariableValueFingerprint,
} from "#/utils/encrypted-secret.server";

const encryptionSecret = "test-encryption-secret";

describe("variable value fingerprints", () => {
  it("keeps sealed fingerprints stable across randomized encryption", () => {
    const value = "secret-token";

    expect(encryptSecretValue(encryptionSecret, value)).not.toEqual(
      encryptSecretValue(encryptionSecret, value),
    );
    expect(getSealedVariableValueFingerprint(encryptionSecret, value)).toBe(
      getSealedVariableValueFingerprint(encryptionSecret, value),
    );
  });

  it("changes sealed fingerprints when the plaintext changes", () => {
    expect(getSealedVariableValueFingerprint(encryptionSecret, "secret-token")).not.toBe(
      getSealedVariableValueFingerprint(encryptionSecret, "other-secret"),
    );
  });

  it("separates plain and sealed fingerprint domains", () => {
    expect(getPlainVariableValueFingerprint("secret-token")).not.toBe(
      getSealedVariableValueFingerprint(encryptionSecret, "secret-token"),
    );
  });
});

// Sealed by the Rust Config Store (`ployz-store`'s sealing) with the same secret; its
// tests open vectors sealed here, so both directions stay byte-compatible.
describe("Config Store sealing vectors", () => {
  const vectors = [
    {
      value: "postgres://u:p@db/app",
      sealed: { version: 1 as const, iv: "SrgYXlvf+kiDClRp", tag: "lWhpMscyOmjBR3lWsTdfyg==", ciphertext: "e8ozWcEBW7AAMvtfIL+n0MrB9P6d" },
      fingerprint: "v1:84deeccda72e54030ebff34a05c985585fa26b1f6e313d5b826f7c1ad916bfdc",
    },
    {
      value: "pässwörd 🔑 秘密",
      sealed: { version: 1 as const, iv: "Dxo7hNTLCyY8lAST", tag: "qHvkFFAxAcX4Zh12J1hhGQ==", ciphertext: "zRX1ENhMjwNnaaog8fSQqeuWu85ISw==" },
      fingerprint: "v1:a5a478b6d3b77dab18f838ae270d88f36135d92dbe6a96423554f13f89db664a",
    },
  ];

  it("opens what Rust sealed and fingerprints alike", () => {
    for (const { value, sealed, fingerprint } of vectors) {
      expect(decryptSecretValue(encryptionSecret, sealed)).toBe(value);
      expect(getSealedVariableValueFingerprint(encryptionSecret, value)).toBe(fingerprint);
    }
  });

  it("rejects a corrupt tag", () => {
    for (const { sealed } of vectors) {
      expect(() => decryptSecretValue(encryptionSecret, { ...sealed, tag: Buffer.alloc(16).toString("base64") })).toThrow();
    }
  });
});
