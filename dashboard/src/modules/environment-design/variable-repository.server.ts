import "@tanstack/react-start/server-only";
import { getPlainVariableValueFingerprint, type SecretEncryptionService } from "#/utils/encrypted-secret.server";
import { Validation } from "#/server/public-error";
import { parseDisplayToParts, type LookupLineage } from "./variable-template";
import type { VariableValueInput } from "./variables";

export function validateVariableValue(
  value: VariableValueInput,
  context: {
    readonly lookupLineage: LookupLineage;
    readonly ownerScope: "service" | "variable_group";
  },
) {
  if (value.type !== "plain") return null;
  const { parts, unresolved } = parseDisplayToParts(value.value, context.lookupLineage);
  if (unresolved.length > 0) {
    const names = [...new Set(unresolved)].join(", ");
    return new Validation({
      field: "value",
      message: `Unknown variable reference${unresolved.length > 1 ? "s" : ""}: ${names}. Check the producer name.`,
    });
  }
  if (
    context.ownerScope === "variable_group" &&
    parts.some((part) => part.kind === "ref" && part.owner.scope !== "self")
  ) {
    return new Validation({
      message: "Variable Groups can only reference their own variables.",
    });
  }
  return null;
}

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
