import { Effect, Schema } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import type {
  EnvironmentSnapshotVariableProducer,
  ValuePart,
  ValuePartRefOwner,
} from "#/modules/environment-design/tables";
import {
  parseEnvironmentResourceNodeConfig,
  type EnvironmentResourceNodeConfigByType,
  type EnvironmentResourceNodeType,
} from "#/modules/environment-design/environment-resource-node";
import {
  projectServiceDeploymentConfig,
  savedServiceIntentConfigEffectSchema,
  valuePartSchema,
  type ServiceDeployEnvValue,
  type ServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";
import {
  isPureLiteral,
  partsToDisplay,
  partsToLiteralString,
  type LookupSlug,
} from "#/modules/environment-design/variable-template";
import { encryptedSecretValueSchema } from "#/modules/environment-design/variables";
import {
  decodeStrict,
  strictParseOptions,
  Uuid,
} from "#/modules/environment-design/schema";
import { Conflict } from "#/server/public-error";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const MutableValueParts = Schema.mutable(Schema.Array(valuePartSchema));

const savedVariableIntentSchema = Schema.Struct({
    id: Uuid,
    key: NonEmptyString,
    description: Schema.NullOr(Schema.String),
    exported: Schema.Boolean,
    valueFingerprint: NonEmptyString,
    value: Schema.Union([
      Schema.Struct({ kind: Schema.Literal("literal"), value: Schema.String }),
      Schema.Struct({
          kind: Schema.Literal("template"),
          parts: MutableValueParts,
        }),
      Schema.Struct({
          kind: Schema.Literal("secret"),
          encryptedValue: encryptedSecretValueSchema,
        }),
    ]),
  });

export const savedServiceIntentConfigSchema =
  savedServiceIntentConfigEffectSchema;

const savedServiceIntentSchema = Schema.Struct({
    id: Uuid,
    lineageId: Uuid,
    slug: NonEmptyString,
    config: savedServiceIntentConfigSchema,
    variables: Schema.mutable(Schema.Array(savedVariableIntentSchema)),
    variableGroupAttachments: Schema.mutable(
      Schema.Array(
        Schema.Struct({ variableGroupId: Uuid, sortOrder: Schema.Int }),
      ),
    ),
    volumeAttachments: Schema.mutable(
      Schema.Array(
        Schema.Struct({
            volumeResourceId: Uuid,
            mountPath: NonEmptyString,
          }),
      ),
    ),
    encryptedRegistryUsername: Schema.NullOr(encryptedSecretValueSchema),
    encryptedRegistrySecret: Schema.NullOr(encryptedSecretValueSchema),
  });

const savedVariableGroupIntentSchema = Schema.Struct({
    resourceId: Uuid,
    resourceLineageId: Uuid,
    variableGroupId: Uuid,
    variableGroupLineageId: Uuid,
    slug: NonEmptyString,
    name: NonEmptyString,
    variables: Schema.mutable(Schema.Array(savedVariableIntentSchema)),
  });

const savedVolumeIntentSchema = Schema.Struct({
    resourceId: Uuid,
    resourceLineageId: Uuid,
    name: NonEmptyString,
  });

const savedEnvironmentIntentBaseSchema = Schema.Struct({
    version: Schema.Literal(1),
    environmentSlug: NonEmptyString,
    services: Schema.mutable(Schema.Array(savedServiceIntentSchema)),
    variableGroups: Schema.mutable(
      Schema.Array(savedVariableGroupIntentSchema),
    ),
    volumes: Schema.mutable(Schema.Array(savedVolumeIntentSchema)),
  });

export const savedEnvironmentIntentSchema = savedEnvironmentIntentBaseSchema.check(
  Schema.makeFilter((intent: typeof savedEnvironmentIntentBaseSchema.Type) => {
    const issues: Schema.FilterIssue[] = [];
    const requireUnique = <T>(
      values: readonly T[],
      key: (value: T) => string,
      path: Array<string | number>,
      label: string,
    ) => {
      const seen = new Set<string>();
      for (const [index, value] of values.entries()) {
        const identity = key(value);
        if (seen.has(identity)) {
          issues.push({
            path: [...path, index],
            issue: `${label} must be unique.`,
          });
        }
        seen.add(identity);
      }
    };
    requireUnique(
      intent.services,
      (service) => service.id,
      ["services"],
      "Service ID",
    );
    requireUnique(
      intent.services,
      (service) => service.lineageId,
      ["services"],
      "Service lineage",
    );
    requireUnique(
      intent.services,
      (service) => service.slug,
      ["services"],
      "Service slug",
    );
    requireUnique(
      intent.variableGroups,
      (group) => group.resourceId,
      ["variableGroups"],
      "Variable Group resource ID",
    );
    requireUnique(
      intent.variableGroups,
      (group) => group.resourceLineageId,
      ["variableGroups"],
      "Variable Group resource lineage",
    );
    requireUnique(
      intent.variableGroups,
      (group) => group.variableGroupId,
      ["variableGroups"],
      "Variable Group ID",
    );
    requireUnique(
      intent.variableGroups,
      (group) => group.variableGroupLineageId,
      ["variableGroups"],
      "Variable Group lineage",
    );
    requireUnique(
      intent.variableGroups,
      (group) => group.slug,
      ["variableGroups"],
      "Variable Group slug",
    );
    requireUnique(
      intent.volumes,
      (volume) => volume.resourceId,
      ["volumes"],
      "Volume ID",
    );
    requireUnique(
      intent.volumes,
      (volume) => volume.resourceLineageId,
      ["volumes"],
      "Volume lineage",
    );
    requireUnique(
      [
        ...intent.variableGroups.map((group) => ({
          id: group.resourceId,
          lineageId: group.resourceLineageId,
        })),
        ...intent.volumes.map((volume) => ({
          id: volume.resourceId,
          lineageId: volume.resourceLineageId,
        })),
      ],
      (resource) => resource.id,
      [],
      "Environment Resource ID",
    );
    requireUnique(
      [
        ...intent.variableGroups.map((group) => ({
          id: group.resourceId,
          lineageId: group.resourceLineageId,
        })),
        ...intent.volumes.map((volume) => ({
          id: volume.resourceId,
          lineageId: volume.resourceLineageId,
        })),
      ],
      (resource) => resource.lineageId,
      [],
      "Environment Resource lineage",
    );
    requireUnique(
      [
        ...intent.services.flatMap((service) => service.variables),
        ...intent.variableGroups.flatMap((group) => group.variables),
      ],
      (variable) => variable.id,
      [],
      "Variable ID",
    );
    const groupIds = new Set(
      intent.variableGroups.map((group) => group.variableGroupId),
    );
    const volumeIds = new Set(
      intent.volumes.map((volume) => volume.resourceId),
    );
    for (const [serviceIndex, service] of intent.services.entries()) {
      requireUnique(
        service.variables,
        (variable) => variable.id,
        ["services", serviceIndex, "variables"],
        "Service variable ID",
      );
      requireUnique(
        service.variables,
        (variable) => variable.key,
        ["services", serviceIndex, "variables"],
        "Service variable key",
      );
      requireUnique(
        service.variableGroupAttachments,
        (attachment) => attachment.variableGroupId,
        ["services", serviceIndex, "variableGroupAttachments"],
        "Variable Group attachment",
      );
      requireUnique(
        service.volumeAttachments,
        (attachment) => attachment.volumeResourceId,
        ["services", serviceIndex, "volumeAttachments"],
        "Volume attachment",
      );
      requireUnique(
        service.volumeAttachments,
        (attachment) => attachment.mountPath,
        ["services", serviceIndex, "volumeAttachments"],
        "Volume mount path",
      );
      for (const [
        attachmentIndex,
        attachment,
      ] of service.variableGroupAttachments.entries()) {
        if (!groupIds.has(attachment.variableGroupId)) {
          issues.push({
            path: [
              "services",
              serviceIndex,
              "variableGroupAttachments",
              attachmentIndex,
            ],
            issue: "Variable Group attachment must reference a Saved node.",
          });
        }
      }
      for (const [
        attachmentIndex,
        attachment,
      ] of service.volumeAttachments.entries()) {
        if (!volumeIds.has(attachment.volumeResourceId)) {
          issues.push({
            path: [
              "services",
              serviceIndex,
              "volumeAttachments",
              attachmentIndex,
            ],
            issue: "Volume attachment must reference a Saved node.",
          });
        }
      }
    }
    for (const [groupIndex, group] of intent.variableGroups.entries()) {
      requireUnique(
        group.variables,
        (variable) => variable.id,
        ["variableGroups", groupIndex, "variables"],
        "Variable Group variable ID",
      );
      requireUnique(
        group.variables,
        (variable) => variable.key,
        ["variableGroups", groupIndex, "variables"],
        "Variable Group variable key",
      );
    }
    return issues;
  }),
);

export type SavedEnvironmentIntent = typeof savedEnvironmentIntentSchema.Type;
export type SavedVariableIntent = typeof savedVariableIntentSchema.Type;
export type CompiledSavedEnvironmentIntent = {
  nodeSnapshots: Array<{
    environmentId: string;
    nodeType: "service" | EnvironmentResourceNodeType;
    nodeId: string;
    nodeLineageId: string;
    configVersion: number;
    config:
      | ServiceDeploymentConfig
      | EnvironmentResourceNodeConfigByType[EnvironmentResourceNodeType];
    encryptedRegistryUsername?: EncryptedSecretValue | null;
    encryptedRegistrySecret?: EncryptedSecretValue | null;
  }>;
  variableProducers: EnvironmentSnapshotVariableProducer[];
};

function envValue(
  variable: SavedVariableIntent,
  lookupSlug: LookupSlug,
  selfOwner: ValuePartRefOwner | null,
  source?: NonNullable<ServiceDeployEnvValue["source"]>,
): ServiceDeployEnvValue {
  if (variable.value.kind === "secret") {
    const value = {
      kind: "secret" as const,
      variableId: variable.id,
      encryptedValue: variable.value.encryptedValue,
      fingerprint: variable.valueFingerprint,
    };
    return source ? { ...value, source } : value;
  }
  if (variable.value.kind === "literal") {
    const value = {
      kind: "literal" as const,
      value: variable.value.value,
    };
    return source ? { ...value, source } : value;
  }
  const parts = variable.value.parts;
  const value = {
    kind: "literal" as const,
    value: partsToDisplay(parts, lookupSlug),
    parts: selfOwner
      ? parts.map((part) =>
          part.kind === "ref" && part.owner.scope === "self"
            ? { ...part, owner: selfOwner }
            : part,
        )
      : parts,
  };
  return source ? { ...value, source } : value;
}

function invariantMapValue<K, V>(map: Map<K, V>, key: K): V {
  const value = map.get(key);
  if (value === undefined)
    throw new Error("Validated Saved relationship is missing.");
  return value;
}

function variableGroupNodeConfig(
  group: SavedEnvironmentIntent["variableGroups"][number],
  lookupSlug: LookupSlug,
) {
  return parseEnvironmentResourceNodeConfig("variable_group", {
    version: 1,
    name: group.name,
    variables: group.variables
      .map((variable) => ({
        key: variable.key,
        description: variable.description,
        exported: variable.exported,
        value:
          variable.value.kind === "secret"
            ? {
                type: "sealed" as const,
                hasValue: true as const,
                fingerprint: variable.valueFingerprint,
                encryptedValue: variable.value.encryptedValue,
              }
            : {
                type: "plain" as const,
                value:
                  variable.value.kind === "literal"
                    ? variable.value.value
                    : partsToDisplay(variable.value.parts, lookupSlug),
              },
      }))
      .sort((left, right) => left.key.localeCompare(right.key)),
  });
}

/**
 * The sole compiler from reviewed authoring intent to deployable material.
 * Saved State never persists the returned Service env, mounts, or producers.
 */
export function compileSavedEnvironmentIntent(input: {
  environmentId: string;
  intent: SavedEnvironmentIntent;
}): CompiledSavedEnvironmentIntent {
  const { intent } = input;
  const slugByLineage = new Map([
    ...intent.services.map(
      (service) => [service.lineageId, service.slug] as const,
    ),
    ...intent.variableGroups.map(
      (group) => [group.variableGroupLineageId, group.slug] as const,
    ),
  ]);
  const lookupSlug: LookupSlug = (lineageId) =>
    slugByLineage.get(lineageId) ?? null;
  const groupById = new Map(
    intent.variableGroups.map((group) => [group.variableGroupId, group]),
  );
  const volumeById = new Map(
    intent.volumes.map((volume) => [volume.resourceId, volume]),
  );

  const serviceNodes: CompiledSavedEnvironmentIntent["nodeSnapshots"] =
    intent.services.map((service) => {
      const env: ServiceDeploymentConfig["env"] = {};
      for (const variable of [...service.variables].sort((left, right) =>
        left.key.localeCompare(right.key),
      )) {
        env[variable.key] = envValue(variable, lookupSlug, null);
      }
      for (const attachment of [...service.variableGroupAttachments].sort(
        (left, right) =>
          left.sortOrder - right.sortOrder ||
          left.variableGroupId.localeCompare(right.variableGroupId),
      )) {
        const group = invariantMapValue(groupById, attachment.variableGroupId);
        for (const variable of [...group.variables]
          .filter((candidate) => candidate.exported)
          .sort((left, right) => left.key.localeCompare(right.key))) {
          env[variable.key] = envValue(
            variable,
            lookupSlug,
            {
              scope: "variable_group",
              lineageId: group.variableGroupLineageId,
            },
            {
              kind: "variable_group",
              resourceId: group.resourceId,
              resourceName: group.name,
              variableGroupId: group.variableGroupId,
              key: variable.key,
            },
          );
        }
      }
      const mounts = [...service.volumeAttachments]
        .sort(
          (left, right) =>
            left.mountPath.localeCompare(right.mountPath) ||
            left.volumeResourceId.localeCompare(right.volumeResourceId),
        )
        .map((attachment) => {
          const volume = invariantMapValue(
            volumeById,
            attachment.volumeResourceId,
          );
          return {
            volumeResourceId: volume.resourceId,
            volumeName: volume.name,
            mountPath: attachment.mountPath,
          };
        });
      return {
        environmentId: input.environmentId,
        nodeType: "service",
        nodeId: service.id,
        nodeLineageId: service.lineageId,
        configVersion: 1,
        config: projectServiceDeploymentConfig({
          ...service.config,
          env,
          mounts,
        }),
        encryptedRegistryUsername: service.encryptedRegistryUsername,
        encryptedRegistrySecret: service.encryptedRegistrySecret,
      };
    });

  const nodeSnapshots: CompiledSavedEnvironmentIntent["nodeSnapshots"] = [
    ...serviceNodes,
    ...intent.variableGroups.map((group) => ({
      environmentId: input.environmentId,
      nodeType: "variable_group" as const,
      nodeId: group.resourceId,
      nodeLineageId: group.resourceLineageId,
      configVersion: 1,
      config: variableGroupNodeConfig(group, lookupSlug),
    })),
    ...intent.volumes.map((volume) => ({
      environmentId: input.environmentId,
      nodeType: "volume" as const,
      nodeId: volume.resourceId,
      nodeLineageId: volume.resourceLineageId,
      configVersion: 2,
      config: parseEnvironmentResourceNodeConfig("volume", {
        version: 2,
        name: volume.name,
      }),
    })),
  ];

  const variableProducers: EnvironmentSnapshotVariableProducer[] = [
    ...intent.services.flatMap((service) => [
      ...getManagedServiceExports({
        id: service.id,
        lineageId: service.lineageId,
        name: service.config.name,
        slug: service.slug,
        environmentId: input.environmentId,
        environmentSlug: intent.environmentSlug,
      }).map((managed) => ({
        ownerScope: "service" as const,
        ownerId: service.id,
        ownerLineageId: service.lineageId,
        key: managed.key,
        value: { kind: "literal" as const, value: managed.value },
      })),
      ...service.variables.map((variable) => ({
        ownerScope: "service" as const,
        ownerId: service.id,
        ownerLineageId: service.lineageId,
        key: variable.key,
        value: variable.value,
      })),
    ]),
    ...intent.variableGroups.flatMap((group) =>
      group.variables.map((variable) => ({
        ownerScope: "variable_group" as const,
        ownerId: group.variableGroupId,
        ownerLineageId: group.variableGroupLineageId,
        key: variable.key,
        value: variable.value,
      })),
    ),
  ].sort((left, right) =>
    `${left.ownerScope}:${left.ownerLineageId}:${left.key}`.localeCompare(
      `${right.ownerScope}:${right.ownerLineageId}:${right.key}`,
    ),
  );

  return { nodeSnapshots, variableProducers };
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

export function canonicalizeSavedEnvironmentIntent(
  input: SavedEnvironmentIntent,
): SavedEnvironmentIntent {
  const intent = structuredClone(input);
  intent.services.sort((left, right) => left.id.localeCompare(right.id));
  intent.variableGroups.sort((left, right) =>
    left.resourceId.localeCompare(right.resourceId),
  );
  intent.volumes.sort((left, right) =>
    left.resourceId.localeCompare(right.resourceId),
  );
  for (const service of intent.services) {
    service.variables.sort((left, right) => left.id.localeCompare(right.id));
    service.variableGroupAttachments.sort(
      (left, right) =>
        left.sortOrder - right.sortOrder ||
        left.variableGroupId.localeCompare(right.variableGroupId),
    );
    service.volumeAttachments.sort(
      (left, right) =>
        left.mountPath.localeCompare(right.mountPath) ||
        left.volumeResourceId.localeCompare(right.volumeResourceId),
    );
  }
  for (const group of intent.variableGroups) {
    group.variables.sort((left, right) => left.id.localeCompare(right.id));
  }
  return intent;
}

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
  return decodeStrict(savedVariableIntentSchema, {
    id: input.id,
    key: input.key,
    description: input.description,
    exported: input.exported,
    valueFingerprint: input.valueFingerprint,
    value,
  });
}
