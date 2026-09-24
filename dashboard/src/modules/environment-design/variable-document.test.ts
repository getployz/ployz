import { expect, it } from "vitest";
import { plainVariableIntent, variableDocumentRecord } from "./variable-document";
import type { SavedEnvironmentIntent } from "./saved-intent";
import type { VariableRecord } from "./variables";

const intent: SavedEnvironmentIntent = { version: 1, environmentSlug: "production", services: [], volumes: [] };
const variable: VariableRecord = {
  id: "00000000-0000-4000-8000-000000000001", serviceId: "00000000-0000-4000-8000-000000000002",
  key: "DATABASE_URL", description: null, exported: false,
  value: { type: "plain", value: "${{ Postgres.DATABASE_URL }}" }, createdAt: new Date(0), updatedAt: new Date(0),
};

it("saves an unknown reference as text and projects a warning after reload", async () => {
  const saved = await plainVariableIntent(variable, intent);
  expect(saved.value).toEqual({ kind: "literal", value: "${{ Postgres.DATABASE_URL }}" });
  const loaded = variableDocumentRecord(saved, variable.serviceId, intent, new Date(0));
  expect(loaded.unresolvedReferences).toEqual(["Postgres"]);
  expect((await plainVariableIntent(loaded, intent)).value).toEqual(saved.value);
  const corrected = await plainVariableIntent({ ...loaded, value: { type: "plain", value: "postgres://database/app" } }, intent);
  expect(variableDocumentRecord(corrected, variable.serviceId, intent, new Date(0)).unresolvedReferences).toEqual([]);
});

it("does not inspect sealed values", () => {
  const saved = { id: variable.id, key: variable.key, description: null, exported: false,
    valueFingerprint: "secret", value: { kind: "secret" as const, encryptedValue: null } };
  expect(variableDocumentRecord(saved, variable.serviceId, intent, new Date(0)).unresolvedReferences).toEqual([]);
});
