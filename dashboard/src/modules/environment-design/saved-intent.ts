import { canonicalJson } from "./canonical-json";
import { Effect, Schema } from "effect";
import {
  parseEnvironmentIntent,
  compileEnvironmentIntent,
  restoreEnvironmentNode,
  type SavedEnvironmentIntent as CoreEnvironmentIntent,
  type SavedServiceIntent as CoreServiceIntent,
  type CompiledEnvironmentNode,
  type VolumeConfig,
} from "@ployz/sdk/config";
import type { EncryptedSecretValue } from "#/db/tables";
import type { ValuePart } from "./tables";
import { savedServiceIntentConfigEffectSchema } from "./services";
import { sharedSchema, type DashboardServiceConfig } from "./service-config";
import { encryptedSecretValueSchema, variableValuePartsSchema } from "./variables";
import { Uuid, decodeStrict, strictParseOptions, type DeepMutable } from "./schema";
import { isPureLiteral, partsToLiteralString, partsToDisplay } from "./variable-template";
import type { VariableGroupConfig } from "./variable-group-config";
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
const attachmentSchema = Schema.Struct({ variableGroupId: Uuid, sortOrder: Schema.Int });
const groupSchema = Schema.Struct({
  resourceId: Uuid, resourceLineageId: Uuid, variableGroupId: Uuid,
  variableGroupLineageId: Uuid, slug: Schema.NonEmptyString, name: Schema.NonEmptyString,
  variables: Schema.Array(savedVariableSchema),
});
export type SavedVariableGroupIntent = DeepMutable<typeof groupSchema.Type>;
export type SavedServiceIntent = Omit<CoreServiceIntent, "variables"> & {
  variables: SavedVariableIntent[];
  variableGroupAttachments: Array<{ variableGroupId: string; sortOrder: number }>;
};
export type SavedEnvironmentIntent = Omit<CoreEnvironmentIntent, "services"> & {
  services: SavedServiceIntent[];
  variableGroups: SavedVariableGroupIntent[];
};

const documentSchema = Schema.Struct({
  version: Schema.Literal(1), environmentSlug: Schema.NonEmptyString,
  services: Schema.Array(Schema.Struct({
    id: Uuid, lineageId: Uuid, slug: Schema.NonEmptyString, config: Schema.Unknown,
    encryptedRegistryUsername: Schema.optionalKey(Schema.NullOr(encryptedSecretValueSchema)),
    encryptedRegistrySecret: Schema.optionalKey(Schema.NullOr(encryptedSecretValueSchema)),
    variables: Schema.Array(savedVariableSchema),
    variableGroupAttachments: Schema.Array(attachmentSchema),
    volumeAttachments: Schema.Array(Schema.Struct({ volumeResourceId: Uuid, mountPath: Schema.NonEmptyString })),
  })),
  variableGroups: Schema.Array(groupSchema),
  volumes: Schema.Array(Schema.Struct({ resourceId: Uuid, resourceLineageId: Uuid, name: Schema.NonEmptyString })),
});

/** Core validates and compiles settings/Volumes; Dashboard owns the variable graph. */
export function coreEnvironmentIntent(intent: SavedEnvironmentIntent): CoreEnvironmentIntent {
  const { variableGroups: _groups, services, ...environment } = intent;
  return { ...environment, services: services.map(({ variableGroupAttachments: _attachments, variables: _variables, ...service }) => ({ ...service, variables: [] })) };
}

function unique(values: string[], label: string) {
  if (new Set(values).size !== values.length) throw new Error(`Duplicate ${label}.`);
}

export function parseDashboardEnvironmentIntent<Input>(value: Input): SavedEnvironmentIntent {
  const wire = decodeStrict(documentSchema, value);
  const { variableGroups, services, ...environment } = wire;
  const core = parseEnvironmentIntent({ ...environment, services: services.map(({ variableGroupAttachments: _attachments, variables: _variables, ...service }) => ({ ...service, variables: [] })) });
  unique(variableGroups.map((g) => g.variableGroupId), "Variable Group identities");
  unique(variableGroups.map((g) => g.variableGroupLineageId), "Variable Group lineages");
  unique(variableGroups.map((g) => g.slug), "Variable Group slugs");
  unique([...variableGroups, ...wire.volumes].map((r) => r.resourceId), "resource identities");
  unique([...variableGroups, ...wire.volumes].map((r) => r.resourceLineageId), "resource lineages");
  const variables = [...services, ...variableGroups].flatMap((owner) => owner.variables);
  unique(variables.map((v) => v.id), "variable identities");
  for (const owner of [...services, ...variableGroups]) unique(owner.variables.map((v) => v.key), "variable keys");
  for (const service of services) {
    unique(service.variableGroupAttachments.map((a) => a.variableGroupId), "Variable Group attachments");
    if (service.variableGroupAttachments.some((a) => !variableGroups.some((g) => g.variableGroupId === a.variableGroupId))) throw new Error("Attachment must reference an authored Variable Group.");
  }
  const authoredServices = new Map(services.map((service) => [service.id, service]));
  return {
    ...core,
    services: core.services.map((service) => {
      const authored = authoredServices.get(service.id);
      if (!authored) throw new Error("Core returned an unknown Service identity.");
      return { ...service,
        // SAFETY: documentSchema validated these fields; cloning removes readonly ownership.
        variables: structuredClone(authored.variables) as SavedVariableIntent[],
        // SAFETY: documentSchema validated these fields; cloning removes readonly ownership.
        variableGroupAttachments: structuredClone(authored.variableGroupAttachments) as SavedServiceIntent["variableGroupAttachments"],
      };
    }),
    // SAFETY: documentSchema validated these fields; cloning removes readonly ownership.
    variableGroups: structuredClone(variableGroups) as SavedVariableGroupIntent[],
  };
}

export function emptyEnvironmentIntent(environmentSlug: string): SavedEnvironmentIntent {
  return { version: 1, environmentSlug, services: [], variableGroups: [], volumes: [] };
}
export const savedServiceIntentConfigSchema = savedServiceIntentConfigEffectSchema;
export const savedEnvironmentIntentSchema = sharedSchema(parseDashboardEnvironmentIntent);

type NodeIdentity = Pick<CompiledEnvironmentNode, "environmentId" | "nodeId" | "nodeLineageId" | "encryptedRegistryUsername" | "encryptedRegistrySecret">;
export type CompiledSavedEnvironmentIntent = {
  nodeSnapshots: Array<NodeIdentity & (
    | { nodeType: "service"; configVersion: 1; config: DashboardServiceConfig }
    | { nodeType: "volume"; configVersion: 2; config: VolumeConfig }
    | { nodeType: "variable_group"; configVersion: 1; config: VariableGroupConfig }
  )>;
  variableProducers: Array<{
    ownerScope: "service" | "variable_group"; ownerId: string; ownerLineageId: string;
    key: string; value: SavedVariableIntent["value"];
  }>;
};

export function compileSavedEnvironmentIntent(input: { environmentId: string; intent: SavedEnvironmentIntent }): CompiledSavedEnvironmentIntent {
  const intent = canonicalizeSavedEnvironmentIntent(input.intent);
  const core = compileEnvironmentIntent(input.environmentId, coreEnvironmentIntent(intent));
  const slugs = new Map([...intent.services.map((s) => [s.lineageId, s.slug] as const), ...intent.variableGroups.map((g) => [g.variableGroupLineageId, g.slug] as const)]);
  const envValue = (variable: SavedVariableIntent, group?: SavedVariableGroupIntent): DashboardServiceConfig["env"][string] => {
    const source = group ? { kind: "variable_group" as const, resourceId: group.resourceId, resourceName: group.name, variableGroupId: group.variableGroupId, key: variable.key } : undefined;
    let result: DashboardServiceConfig["env"][string];
    if (variable.value.kind === "secret") {
      result = { kind: "secret", variableId: variable.id, fingerprint: variable.valueFingerprint };
      if (variable.value.encryptedValue) result.encryptedValue = variable.value.encryptedValue;
    } else if (variable.value.kind === "literal") {
      result = { kind: "literal", value: variable.value.value };
    } else {
      const parts = variable.value.parts.map((part): ValuePart => part.kind === "ref" && part.owner.scope === "self" && group
        ? { ...part, owner: { scope: "variable_group", lineageId: group.variableGroupLineageId } } : part);
      result = { kind: "literal", value: partsToDisplay(variable.value.parts, (id) => slugs.get(id) ?? null), parts };
    }
    if (source) result.source = source;
    return result;
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
    const env: DashboardServiceConfig["env"] = Object.fromEntries(service.variables.map((v) => [v.key, envValue(v)]));
    for (const attachment of service.variableGroupAttachments) {
      const group = intent.variableGroups.find((g) => g.variableGroupId === attachment.variableGroupId);
      if (!group) throw new Error("Attachment must reference an authored Variable Group.");
      for (const variable of group.variables) if (variable.exported) env[variable.key] = envValue(variable, group);
    }
    return { ...node, nodeType: "service", configVersion: 1, config: { ...node.config, env, variableGroupAttachments: service.variableGroupAttachments } };
  });
  const variableProducers: CompiledSavedEnvironmentIntent["variableProducers"] = [...core.variableProducers];
  for (const service of intent.services) for (const variable of service.variables) variableProducers.push({ ownerScope: "service", ownerId: service.id, ownerLineageId: service.lineageId, key: variable.key, value: variable.value });
  for (const group of intent.variableGroups) {
    nodeSnapshots.push({ environmentId: input.environmentId, nodeId: group.resourceId, nodeLineageId: group.resourceLineageId, nodeType: "variable_group", configVersion: 1,
      config: { version: 1, name: group.name, variables: group.variables.map((v): VariableGroupConfig["variables"][number] => {
        let value: VariableGroupConfig["variables"][number]["value"];
        if (v.value.kind === "secret") {
          value = { type: "sealed", hasValue: true, fingerprint: v.valueFingerprint };
          if (v.value.encryptedValue) value = { ...value, encryptedValue: v.value.encryptedValue };
        } else {
          value = { type: "plain", value: v.value.kind === "literal" ? v.value.value : partsToDisplay(v.value.parts, (id) => slugs.get(id) ?? null) };
        }
        return { key: v.key, description: v.description, exported: v.exported, value };
      }).sort((a, b) => compareText(a.key, b.key)) } });
    for (const variable of group.variables) variableProducers.push({ ownerScope: "variable_group", ownerId: group.variableGroupId, ownerLineageId: group.variableGroupLineageId, key: variable.key, value: variable.value });
  }
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
  next.variableGroups.sort((a, b) => compareText(a.resourceId, b.resourceId));
  next.volumes.sort((a, b) => compareText(a.resourceId, b.resourceId));
  for (const owner of [...next.services, ...next.variableGroups]) owner.variables.sort((a, b) => compareText(a.id, b.id));
  for (const service of next.services) {
    service.variableGroupAttachments.sort((a, b) => a.sortOrder - b.sortOrder || compareText(a.variableGroupId, b.variableGroupId));
    service.volumeAttachments.sort((a, b) => compareText(a.mountPath, b.mountPath) || compareText(a.volumeResourceId, b.volumeResourceId));
  }
  return next;
}
export function redactSavedEnvironmentIntent(intent: SavedEnvironmentIntent): SavedEnvironmentIntent {
  const next = structuredClone(intent);
  for (const service of next.services) { service.encryptedRegistryUsername = null; service.encryptedRegistrySecret = null; }
  for (const owner of [...next.services, ...next.variableGroups]) for (const variable of owner.variables) if (variable.value.kind === "secret") variable.value.encryptedValue = null;
  return next;
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
  node: { nodeType: "service" | "variable_group" | "volume"; nodeId: string }, path?: string): SavedEnvironmentIntent {
  const next = structuredClone(current);
  if (node.nodeType === "variable_group") {
    if (path) throw new Error("Resource settings restore with their owner.");
    const existing = next.variableGroups.find((g) => g.resourceId === node.nodeId);
    const prior = baseline?.variableGroups.find((g) => g.resourceId === node.nodeId);
    next.variableGroups = next.variableGroups.filter((g) => g.resourceId !== node.nodeId);
    if (prior) next.variableGroups.push(structuredClone(prior));
    for (const service of next.services) {
      if (!prior && existing) service.variableGroupAttachments = service.variableGroupAttachments.filter((a) => a.variableGroupId !== existing.variableGroupId);
      else if (prior && !existing) {
        const attachment = baseline?.services.find((s) => s.id === service.id)?.variableGroupAttachments.find((a) => a.variableGroupId === prior.variableGroupId);
        if (attachment && !service.variableGroupAttachments.some((a) => a.variableGroupId === prior.variableGroupId)) service.variableGroupAttachments.push(structuredClone(attachment));
      }
    }
  } else if (node.nodeType === "service" && !path) {
    next.services = next.services.filter((s) => s.id !== node.nodeId);
    const prior = baseline?.services.find((s) => s.id === node.nodeId);
    if (prior) next.services.push(structuredClone(prior));
  } else if (node.nodeType === "service" && path === "variableGroupAttachments") {
    const service = next.services.find((s) => s.id === node.nodeId);
    const prior = baseline?.services.find((s) => s.id === node.nodeId);
    if (!service || !prior) throw new Error("Authored baseline is unavailable.");
    service.variableGroupAttachments = structuredClone(prior.variableGroupAttachments);
  } else {
    const restored = restoreEnvironmentNode(coreEnvironmentIntent(next), baseline ? coreEnvironmentIntent(baseline) : null, { nodeType: node.nodeType, nodeId: node.nodeId }, path);
    next.volumes = restored.volumes;
    next.services = restored.services.map((service) => {
      const authored = next.services.find((s) => s.id === service.id);
      if (!authored) throw new Error("Restored Service has no authored owner.");
      return { ...service, variables: authored.variables, variableGroupAttachments: authored.variableGroupAttachments };
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
