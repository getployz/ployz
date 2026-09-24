import "@tanstack/react-start/server-only";
import { getPlainVariableValueFingerprint, type SecretEncryptionService } from "#/utils/encrypted-secret.server";
import { parseDisplayToParts, type LookupLineage } from "./variable-template";
import type { VariableValueInput } from "./variables";

export function variableValueColumnsForWrite(
  encryption: SecretEncryptionService,
  value: VariableValueInput,
  lookupLineage: LookupLineage,
) {
  if (value.type === "sealed") {
    return {
      valueKind: "sealed" as const,
      valueParts: null,
      encryptedValue: encryption.encrypt(value.value),
      valueFingerprint: encryption.sealedFingerprint(value.value),
    };
  }
  const { parts } = parseDisplayToParts(value.value, lookupLineage);
  return {
    valueKind: "plain" as const,
    valueParts: parts,
    encryptedValue: null,
    valueFingerprint: getPlainVariableValueFingerprint(JSON.stringify(parts)),
  };
}
