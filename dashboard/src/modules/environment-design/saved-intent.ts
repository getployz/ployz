import { Effect, Schema } from "effect";
import {
  parseEnvironmentIntent,
  parseSavedVariable,
  canonicalizeEnvironmentIntent,
  compileEnvironmentIntent,
  type SavedEnvironmentIntent,
  type SavedVariableIntent,
  type CompiledEnvironmentIntent,
} from "@ployz/sdk/config";
import type { EncryptedSecretValue } from "#/db/tables";
import type { ValuePart } from "#/modules/environment-design/tables";
import { savedServiceIntentConfigEffectSchema } from "#/modules/environment-design/services";
import { sharedSchema } from "#/modules/environment-design/service-config";
import { isPureLiteral, partsToLiteralString } from "#/modules/environment-design/variable-template";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { Conflict } from "#/server/public-error";

export type { SavedEnvironmentIntent, SavedVariableIntent };
export function emptyEnvironmentIntent(environmentSlug: string): SavedEnvironmentIntent {
  return { version: 1, environmentSlug, services: [], variableGroups: [], volumes: [] };
}
export type CompiledSavedEnvironmentIntent = CompiledEnvironmentIntent;
export const savedServiceIntentConfigSchema = savedServiceIntentConfigEffectSchema;
export const savedEnvironmentIntentSchema = sharedSchema(parseEnvironmentIntent);

export function compileSavedEnvironmentIntent(input: {
  environmentId: string;
  intent: SavedEnvironmentIntent;
}): CompiledSavedEnvironmentIntent {
  return compileEnvironmentIntent(input.environmentId, input.intent);
}

export const decodePersistedSavedEnvironmentIntent = Effect.fn(
  "EnvironmentDesign.decodePersistedSavedEnvironmentIntent",
)(function* <T>(intent: T) {
  return yield* Schema.decodeUnknownEffect(savedEnvironmentIntentSchema)(
    intent,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () => new Conflict({ message: "Saved Environment State is invalid." }),
    ),
  );
});

export const decodePersistedSavedEnvironmentState = Effect.fn(
  "EnvironmentDesign.decodePersistedSavedEnvironmentState",
)(function* (input: {
  environmentId: string;
  intent: unknown;
}) {
  const intent = yield* decodePersistedSavedEnvironmentIntent(input.intent);
  return {
    intent,
    ...compileSavedEnvironmentIntent({
      environmentId: input.environmentId,
      intent,
    }),
  };
});

export const canonicalizeSavedEnvironmentIntent = canonicalizeEnvironmentIntent;

export function encodePersistedSavedEnvironmentIntent(input: {
  intent: SavedEnvironmentIntent;
}) {
  const intent = canonicalizeSavedEnvironmentIntent(input.intent);
  return { intent };
}

export function savedVariableIntent(input: {
  id: string;
  key: string;
  description: string | null;
  exported: boolean;
  valueKind: "plain" | "sealed";
  valueParts: ValuePart[] | null;
  encryptedValue: EncryptedSecretValue | null;
  valueFingerprint: string;
}): SavedVariableIntent {
  const value =
    input.valueKind === "sealed"
      ? input.encryptedValue
        ? { kind: "secret" as const, encryptedValue: input.encryptedValue }
        : null
      : isPureLiteral(input.valueParts ?? [])
        ? {
            kind: "literal" as const,
            value: partsToLiteralString(input.valueParts ?? []) ?? "",
          }
        : { kind: "template" as const, parts: input.valueParts ?? [] };
  if (!value)
    throw new Error(`Secret variable ${input.key} has no saved value.`);
  return parseSavedVariable({
    id: input.id,
    key: input.key,
    description: input.description,
    exported: input.exported,
    valueFingerprint: input.valueFingerprint,
    value,
  });
}
