import { describe, expect, it } from "vitest";
import {
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
