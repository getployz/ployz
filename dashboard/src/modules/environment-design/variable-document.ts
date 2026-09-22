import type { SavedEnvironmentIntent, SavedVariableIntent } from "./saved-intent";
import type { VariableRecord } from "./variables";
import { savedVariableIntent } from "./saved-intent";
import { extractDisplayRefs, parseDisplayToParts, partsToDisplay } from "./variable-template";

export function environmentVariableReferences(intent: SavedEnvironmentIntent) {
  const owners = [
    ...intent.services.map((node) => ({ lineageId: node.lineageId, slug: node.slug, scope: "service" as const })),
    ...intent.variableGroups.map((node) => ({ lineageId: node.variableGroupLineageId, slug: node.slug, scope: "variable_group" as const })),
  ];
  return {
    lookupSlug: (lineageId: string) => owners.find((owner) => owner.lineageId === lineageId)?.slug ?? null,
    lookupLineage: (slug: string) => owners.find((owner) => owner.slug === slug) ?? null,
  };
}

export function variableDocumentRecord(
  variable: SavedVariableIntent,
  owner: { serviceId: string | null; variableGroupId: string | null },
  intent: SavedEnvironmentIntent,
  updatedAt: Date,
): VariableRecord {
  const references = environmentVariableReferences(intent);
  const parts = variable.value.kind === "secret" ? []
    : variable.value.kind === "literal" ? [{ kind: "text" as const, value: variable.value.value }] : variable.value.parts;
  const unresolvedReferences = [...new Set(parts.flatMap((part) => part.kind === "text"
    ? extractDisplayRefs(part.value).flatMap((ref) => ref.ownerSlug && !references.lookupLineage(ref.ownerSlug) ? [ref.ownerSlug] : [])
    : []))];
  return { unresolvedReferences, id: variable.id, serviceId: owner.serviceId, variableGroupId: owner.variableGroupId, key: variable.key, description: variable.description, exported: variable.exported,
    value: variable.value.kind === "secret" ? { type: "sealed", hasValue: true, fingerprint: variable.valueFingerprint }
      : { type: "plain", value: partsToDisplay(parts, references.lookupSlug) },
    createdAt: updatedAt, updatedAt,
  };
}

export async function plainVariableIntent(variable: VariableRecord, intent: SavedEnvironmentIntent): Promise<SavedVariableIntent> {
  if (variable.value.type !== "plain") throw new Error("Use the sealed-variable action to edit a secret.");
  const { parts } = parseDisplayToParts(variable.value.value, environmentVariableReferences(intent).lookupLineage);
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify({ kind: "plain", value: JSON.stringify(parts) })));
  const fingerprint = `v1:${Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
  return savedVariableIntent({ id: variable.id, key: variable.key, description: variable.description, exported: variable.exported,
    valueFingerprint: fingerprint, valueKind: "plain", valueParts: parts, encryptedValue: null });
}
