import { canonicalJson } from "./canonical-json";
import { Effect, Schema } from "effect";
import {
  parseEnvironmentIntent,
  compileEnvironmentIntent,
  restoreEnvironmentNode,
  type SavedEnvironmentIntent as CoreEnvironmentIntent,
  type SavedServiceIntent as CoreServiceIntent,
  type CompiledEnvironmentNode,
  type ServiceConfig,
  type VolumeConfig,
} from "@ployz/sdk/config";
import type { EncryptedSecretValue, JsonValue } from "#/db/tables";
import type { ValuePart } from "./tables";
import { savedServiceIntentConfigEffectSchema } from "./services";
import { sharedSchema } from "./service-config";
import { encryptedSecretValueSchema, variableValuePartsSchema } from "./variables";
import { Uuid, decodeStrict, strictParseOptions, type DeepMutable } from "./schema";
import { isPureLiteral, partsToLiteralString, partsToDisplay } from "./variable-template";
import { Conflict } from "#/server/public-error";

const nullableDescription = Schema.NullOr(Schema.String).pipe(Schema.withDecodingDefaultKey(Effect.succeed(null)));
const savedVariableSchema = Schema.Struct({
  id: Uuid, key: Schema.NonEmptyString, description: nullableDescription,
  exported: Schema.Boolean, valueFingerprint: Schema.NonEmptyString,
  value: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("literal"), value: Schema.String }),
    Schema.Struct({ kind: Schema.Literal("template"), parts: variableValuePartsSchema }),
    Schema.Struct({ kind: Schema.Literal("secret"), encryptedValue: Schema.NullOr(encryptedSecretValueSchema).pipe(Schema.withDecodingDefaultKey(Effect.succeed(null))) }),
  ]),
});
export type SavedVariableIntent = DeepMutable<typeof savedVariableSchema.Type>;
export type SavedServiceIntent = Omit<CoreServiceIntent, "variables"> & {
  variables: SavedVariableIntent[];
};
export type SavedEnvironmentIntent = Omit<CoreEnvironmentIntent, "services"> & {
  services: SavedServiceIntent[];
};

const documentSchema = Schema.Struct({
  version: Schema.Literal(1), environmentSlug: Schema.NonEmptyString,
  services: Schema.Array(Schema.Struct({
    id: Uuid, lineageId: Uuid, slug: Schema.NonEmptyString, config: Schema.Unknown,
    variables: Schema.Array(savedVariableSchema),
    volumeAttachments: Schema.Array(Schema.Struct({ volumeResourceId: Uuid, mountPath: Schema.NonEmptyString })),
  })),
  volumes: Schema.Array(Schema.Struct({ resourceId: Uuid, resourceLineageId: Uuid, name: Schema.NonEmptyString })),
});

/** Core validates and compiles settings/Volumes; Dashboard owns the variable graph. */
export function coreEnvironmentIntent(intent: SavedEnvironmentIntent): CoreEnvironmentIntent {
  const { services, ...environment } = intent;
  return { ...environment, services: services.map(({ variables: _variables, ...service }) => ({ ...service, variables: [] })) };
}

function unique(values: string[], label: string) {
  if (new Set(values).size !== values.length) throw new Error(`Duplicate ${label}.`);
}

export function parseDashboardEnvironmentIntent<Input>(value: Input): SavedEnvironmentIntent {
  const wire = decodeStrict(documentSchema, value);
  const { services, ...environment } = wire;
  const core = parseEnvironmentIntent({ ...environment, services: services.map(({ variables: _variables, ...service }) => ({ ...service, variables: [] })) });
  unique(wire.volumes.map((r) => r.resourceId), "resource identities");
  unique(wire.volumes.map((r) => r.resourceLineageId), "resource lineages");
  unique(services.flatMap((owner) => owner.variables).map((v) => v.id), "variable identities");
  for (const owner of services) unique(owner.variables.map((v) => v.key), "variable keys");
  const authoredServices = new Map(services.map((service) => [service.id, service]));
  return {
    ...core,
    services: core.services.map((service) => {
      const authored = authoredServices.get(service.id);
      if (!authored) throw new Error("Core returned an unknown Service identity.");
      return { ...service,
        // SAFETY: documentSchema validated these fields; cloning removes readonly ownership.
        variables: structuredClone(authored.variables) as SavedVariableIntent[],
      };
    }),
  };
}

export function emptyEnvironmentIntent(environmentSlug: string): SavedEnvironmentIntent {
  return { version: 1, environmentSlug, services: [], volumes: [] };
}
export const savedServiceIntentConfigSchema = savedServiceIntentConfigEffectSchema;
export const savedEnvironmentIntentSchema = sharedSchema(parseDashboardEnvironmentIntent);

type NodeIdentity = Pick<CompiledEnvironmentNode, "environmentId" | "nodeId" | "nodeLineageId" | "encryptedRegistryUsername" | "encryptedRegistrySecret">;
export type CompiledSavedEnvironmentIntent = {
  nodeSnapshots: Array<NodeIdentity & (
    | { nodeType: "service"; configVersion: 1; config: ServiceConfig }
    | { nodeType: "volume"; configVersion: 2; config: VolumeConfig }
  )>;
  variableProducers: Array<{
    ownerScope: "service"; ownerId: string; ownerLineageId: string;
    key: string; value: SavedVariableIntent["value"];
  }>;
};

export function compileSavedEnvironmentIntent(input: { environmentId: string; intent: SavedEnvironmentIntent }): CompiledSavedEnvironmentIntent {
  const intent = canonicalizeSavedEnvironmentIntent(input.intent);
  const core = compileEnvironmentIntent(input.environmentId, coreEnvironmentIntent(intent));
  const slugs = new Map(intent.services.map((s) => [s.lineageId, s.slug] as const));
  const envValue = (variable: SavedVariableIntent): ServiceConfig["env"][string] => {
    if (variable.value.kind === "secret") {
      const result: ServiceConfig["env"][string] = { kind: "secret", variableId: variable.id, fingerprint: variable.valueFingerprint };
      if (variable.value.encryptedValue) result.encryptedValue = variable.value.encryptedValue;
      return result;
    }
    if (variable.value.kind === "literal") return { kind: "literal", value: variable.value.value };
    return { kind: "literal", value: partsToDisplay(variable.value.parts, (id) => slugs.get(id) ?? null), parts: variable.value.parts };
  };

  const nodeSnapshots: CompiledSavedEnvironmentIntent["nodeSnapshots"] = core.nodeSnapshots.map((node) => {
    if (node.nodeType === "volume" && !("source" in node.config)) {
      return { ...node, nodeType: "volume", configVersion: 2, config: node.config };
    }
    if (node.nodeType !== "service" || !("source" in node.config)) {
      throw new Error("Compiled node type does not match its configuration.");
    }
    const service = intent.services.find((s) => s.id === node.nodeId);
    if (!service) throw new Error("Compiled Service has no authored owner.");
    const env: ServiceConfig["env"] = Object.fromEntries(service.variables.map((v) => [v.key, envValue(v)]));
    return { ...node, nodeType: "service", configVersion: 1, config: { ...node.config, env } };
  });
  const variableProducers: CompiledSavedEnvironmentIntent["variableProducers"] = [...core.variableProducers];
  for (const service of intent.services) for (const variable of service.variables) variableProducers.push({ ownerScope: "service", ownerId: service.id, ownerLineageId: service.lineageId, key: variable.key, value: variable.value });
  variableProducers.sort((a, b) => compareText(a.ownerScope, b.ownerScope) || compareText(a.ownerLineageId, b.ownerLineageId) || compareText(a.key, b.key));
  return { nodeSnapshots, variableProducers };
}

export const decodePersistedSavedEnvironmentIntent = Effect.fn("EnvironmentDesign.decodePersistedSavedEnvironmentIntent")(function* <Input>(intent: Input) {
  return yield* Schema.decodeUnknownEffect(savedEnvironmentIntentSchema)(intent, strictParseOptions).pipe(Effect.mapError(() => new Conflict({ message: "Saved Environment State is invalid." })));
});
export const decodePersistedSavedEnvironmentState = Effect.fn("EnvironmentDesign.decodePersistedSavedEnvironmentState")(function* (input: { environmentId: string; intent: unknown }) {
  const intent = yield* decodePersistedSavedEnvironmentIntent(input.intent);
  return { intent, ...compileSavedEnvironmentIntent({ environmentId: input.environmentId, intent }) };
});

function compareText(a: string, b: string) { return a < b ? -1 : a > b ? 1 : 0; }
export function canonicalizeSavedEnvironmentIntent(intent: SavedEnvironmentIntent): SavedEnvironmentIntent {
  const next = structuredClone(intent);
  next.services.sort((a, b) => compareText(a.id, b.id));
  next.volumes.sort((a, b) => compareText(a.resourceId, b.resourceId));
  for (const service of next.services) {
    service.variables.sort((a, b) => compareText(a.id, b.id));
    service.volumeAttachments.sort((a, b) => compareText(a.mountPath, b.mountPath) || compareText(a.volumeResourceId, b.volumeResourceId));
  }
  return next;
}
export function redactSavedEnvironmentIntent(intent: SavedEnvironmentIntent): SavedEnvironmentIntent {
  const next = structuredClone(intent);
  for (const owner of next.services) for (const variable of owner.variables) if (variable.value.kind === "secret") variable.value.encryptedValue = null;
  return next;
}
/** Drops every sealed `encryptedValue` from a JSON value bound for the browser; a sealed value still reads as `kind: "secret"` with its fingerprint. */
export function withoutSealedCiphertext<Value>(value: Value): Value {
  // SAFETY: a JSON round trip of JSON data returns the same shape minus the dropped key.
  return JSON.parse(JSON.stringify(value, (key: string, entry: JsonValue) => key === "encryptedValue" ? undefined : entry)) as Value;
}
export function encodePersistedSavedEnvironmentIntent(input: { intent: SavedEnvironmentIntent }) { return { intent: canonicalizeSavedEnvironmentIntent(input.intent) }; }
export function reuseSavedEnvironmentPublication(input: {
  policy: "always_create" | "reuse_latest_if_equivalent";
  current: { intent: SavedEnvironmentIntent; volumeDeletionAuthorizations: unknown };
  latest: { intent: SavedEnvironmentIntent; volumeDeletionAuthorizations: unknown } | null;
}) {
  if (input.policy === "always_create" || !input.latest) return false;

  return canonicalJson(canonicalizeSavedEnvironmentIntent(parseDashboardEnvironmentIntent(input.current.intent))) === canonicalJson(canonicalizeSavedEnvironmentIntent(parseDashboardEnvironmentIntent(input.latest.intent)))
    && canonicalJson(input.current.volumeDeletionAuthorizations) === canonicalJson(input.latest.volumeDeletionAuthorizations);
}

export function restoreDashboardEnvironmentNode(current: SavedEnvironmentIntent, baseline: SavedEnvironmentIntent | null,
  node: { nodeType: "service" | "volume"; nodeId: string }, path?: string): SavedEnvironmentIntent {
  const next = structuredClone(current);
  if (node.nodeType === "service" && !path) {
    next.services = next.services.filter((s) => s.id !== node.nodeId);
    const prior = baseline?.services.find((s) => s.id === node.nodeId);
    if (prior) next.services.push(structuredClone(prior));
  } else {
    const restored = restoreEnvironmentNode(coreEnvironmentIntent(next), baseline ? coreEnvironmentIntent(baseline) : null, { nodeType: node.nodeType, nodeId: node.nodeId }, path);
    next.volumes = restored.volumes;
    next.services = restored.services.map((service) => {
      const authored = next.services.find((s) => s.id === service.id);
      if (!authored) throw new Error("Restored Service has no authored owner.");
      return { ...service, variables: authored.variables };
    });
  }
  return canonicalizeSavedEnvironmentIntent(parseDashboardEnvironmentIntent(next));
}

export function savedVariableIntent(input: { id: string; key: string; description: string | null; exported: boolean; valueKind: "plain" | "sealed"; valueParts: ValuePart[] | null; encryptedValue: EncryptedSecretValue | null; valueFingerprint: string }): SavedVariableIntent {
  const value = input.valueKind === "sealed"
    ? input.encryptedValue ? { kind: "secret" as const, encryptedValue: input.encryptedValue } : null
    : isPureLiteral(input.valueParts ?? []) ? { kind: "literal" as const, value: partsToLiteralString(input.valueParts ?? []) ?? "" }
    : { kind: "template" as const, parts: input.valueParts ?? [] };
  if (!value) throw new Error(`Secret variable ${input.key} has no saved value.`);
  // SAFETY: savedVariableSchema validates the value; cloning permits mutable domain ownership.
  return structuredClone(decodeStrict(savedVariableSchema, { id: input.id, key: input.key, description: input.description, exported: input.exported, valueFingerprint: input.valueFingerprint, value })) as SavedVariableIntent;
}
