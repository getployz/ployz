import "@tanstack/react-start/server-only";
import { and, eq, inArray, max, or } from "drizzle-orm";
import { Effect } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import {
  configKey,
  configValue,
  environmentVariableGroup,
  service,
  serviceVariableGroupAttachment,
  variable,
  variableSecret,
} from "#/modules/environment-design/tables";
import type { ValuePart } from "#/modules/environment-design/tables";
import { environment } from "#/modules/project/tables";
import {
  organizationIdForProject,
  organizationIdForService,
} from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";
import {
  getPlainVariableValueFingerprint,
  type SecretEncryptionService,
} from "#/utils/encrypted-secret.server";
import {
  parseDisplayToParts,
  partsToDisplay,
  type LookupLineage,
  type LookupSlug,
} from "#/modules/environment-design/variable-template";
import { decodeStrict } from "./schema";
import {
  encryptedSecretValueSchema,
  environmentVariableGroupSelectSchema,
  variableSelectSchema,
  variableValuePartsSchema,
  type EnvironmentVariableGroupRecord,
  type VariableRecord,
  type VariableValueInput,
} from "./variables";

export type ConfigKeyOwner =
  | { readonly scope: "service_lineage"; readonly lineageId: string }
  | { readonly scope: "variable_group_lineage"; readonly lineageId: string };

const variableBaseColumns = {
  id: variable.id,
  projectId: variable.projectId,
  serviceId: variable.serviceId,
  variableGroupId: variable.variableGroupId,
  configKeyId: variable.configKeyId,
  key: variable.key,
  description: variable.description,
  exported: variable.exported,
  valueKind: variable.valueKind,
  valueParts: variable.valueParts,
  valueFingerprint: variable.valueFingerprint,
  createdAt: variable.createdAt,
  updatedAt: variable.updatedAt,
};

const variableColumns = {
  ...variableBaseColumns,
  encryptedValue: variableSecret.encryptedValue,
};

const variableGroupColumns = {
  id: environmentVariableGroup.id,
  projectId: environmentVariableGroup.projectId,
  environmentId: environmentVariableGroup.environmentId,
  lineageId: environmentVariableGroup.lineageId,
  name: environmentVariableGroup.name,
  slug: environmentVariableGroup.slug,
  createdAt: environmentVariableGroup.createdAt,
  updatedAt: environmentVariableGroup.updatedAt,
};

export type VariableRow = {
  readonly id: string;
  readonly projectId: string;
  readonly serviceId: string | null;
  readonly variableGroupId: string | null;
  readonly configKeyId: string;
  readonly key: string;
  readonly description: string | null;
  readonly exported: boolean;
  readonly valueKind: "plain" | "sealed";
  readonly valueParts: ValuePart[] | null;
  readonly valueFingerprint: string;
  readonly encryptedValue: EncryptedSecretValue | null;
  readonly createdAt: Date;
  readonly updatedAt: Date;
};

interface VariableWriteValues {
  readonly id?: string;
  readonly serviceId: string | null;
  readonly variableGroupId: string | null;
  readonly key: string;
  readonly description: string | null;
  readonly exported: boolean;
  readonly valueKind: "plain" | "sealed";
  readonly valueParts: ValuePart[] | null;
  readonly valueFingerprint: string;
  readonly encryptedValue: EncryptedSecretValue | null;
}

interface EnvironmentRefIndex {
  readonly lookupSlug: LookupSlug;
  readonly lookupLineage: LookupLineage;
}

function buildRefIndex(
  rows: ReadonlyArray<{
    readonly lineageId: string;
    readonly slug: string;
    readonly scope: "service" | "variable_group";
  }>,
): EnvironmentRefIndex {
  const slugByLineage = new Map<string, string>();
  const ownerBySlug = new Map<
    string,
    { lineageId: string; scope: "service" | "variable_group" }
  >();
  for (const row of rows) {
    slugByLineage.set(row.lineageId, row.slug);
    if (!ownerBySlug.has(row.slug)) {
      ownerBySlug.set(row.slug, { lineageId: row.lineageId, scope: row.scope });
    }
  }
  return {
    lookupSlug: (lineageId) => slugByLineage.get(lineageId) ?? null,
    lookupLineage: (slug) => ownerBySlug.get(slug) ?? null,
  };
}

export const loadEnvironmentRefIndex = Effect.fn(
  "EnvironmentDesign.loadEnvironmentRefIndex",
)(function* (environmentId: string) {
  const database = yield* Database;
  const [services, variableGroups] = yield* Effect.all(
    [
      database.drizzle
        .select({ lineageId: service.lineageId, slug: service.slug })
        .from(service)
        .where(eq(service.environmentId, environmentId)),
      database.drizzle
        .select({
          lineageId: environmentVariableGroup.lineageId,
          slug: environmentVariableGroup.slug,
        })
        .from(environmentVariableGroup)
        .where(eq(environmentVariableGroup.environmentId, environmentId)),
    ],
    { concurrency: "unbounded" },
  );
  return buildRefIndex([
    ...services.map((row) => ({ ...row, scope: "service" as const })),
    ...variableGroups.map((row) => ({
      ...row,
      scope: "variable_group" as const,
    })),
  ]);
});

function decodeVariableRow(row: VariableRow): VariableRow {
  return {
    ...row,
    valueParts:
      row.valueParts === null
        ? null
        : [...decodeStrict(variableValuePartsSchema, row.valueParts)],
    encryptedValue:
      row.encryptedValue === null
        ? null
        : decodeStrict(encryptedSecretValueSchema, row.encryptedValue),
  };
}

function toVariableRecord(
  raw: VariableRow,
  lookupSlug: LookupSlug = () => null,
): VariableRecord {
  const row = decodeVariableRow(raw);
  return decodeStrict(variableSelectSchema, {
    id: row.id,
    serviceId: row.serviceId,
    variableGroupId: row.variableGroupId,
    configKeyId: row.configKeyId,
    key: row.key,
    description: row.description,
    exported: row.exported,
    value:
      row.valueKind === "plain"
        ? { type: "plain", value: partsToDisplay(row.valueParts ?? [], lookupSlug) }
        : {
            type: "sealed",
            hasValue: true,
            fingerprint: row.encryptedValue === null
              ? "missing"
              : row.valueFingerprint,
          },
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
  });
}

export const getServiceVariableOwner = Effect.fn(
  "EnvironmentDesign.getServiceVariableOwner",
)(function* (environmentId: string, serviceId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      id: service.id,
      projectId: service.projectId,
      environmentId: service.environmentId,
      lineageId: service.lineageId,
      name: service.name,
      slug: service.slug,
      environmentSlug: environment.namespace,
    })
    .from(service)
    .innerJoin(environment, eq(service.environmentId, environment.id))
    .where(and(eq(service.environmentId, environmentId), eq(service.id, serviceId)))
    .limit(1);
  return rows[0] ?? null;
});

export const getVariableGroupOwner = Effect.fn(
  "EnvironmentDesign.getVariableGroupOwner",
)(function* (environmentId: string, variableGroupId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(variableGroupColumns)
    .from(environmentVariableGroup)
    .where(
      and(
        eq(environmentVariableGroup.environmentId, environmentId),
        eq(environmentVariableGroup.id, variableGroupId),
      ),
    )
    .limit(1);
  return rows[0] === undefined
    ? null
    : decodeStrict(environmentVariableGroupSelectSchema, rows[0]);
});

export const listVariablesForGroups = Effect.fn(
  "EnvironmentDesign.listVariablesForGroups",
)(function* (variableGroupIds: readonly string[]) {
  if (variableGroupIds.length === 0) {
    return new Map<string, VariableRecord[]>();
  }
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(variableColumns)
    .from(variable)
    .leftJoin(variableSecret, eq(variableSecret.variableId, variable.id))
    .where(inArray(variable.variableGroupId, [...variableGroupIds]))
    .orderBy(variable.key);
  const byGroup = new Map<string, VariableRecord[]>();
  for (const row of rows) {
    if (row.variableGroupId === null) continue;
    const records = byGroup.get(row.variableGroupId) ?? [];
    records.push(toVariableRecord(row));
    byGroup.set(row.variableGroupId, records);
  }
  return byGroup;
});

function configKeyOwnerCondition(owner: ConfigKeyOwner) {
  return owner.scope === "service_lineage"
    ? and(
        eq(configKey.scope, "service_lineage"),
        eq(configKey.serviceLineageId, owner.lineageId),
      )
    : and(
        eq(configKey.scope, "variable_group_lineage"),
        eq(configKey.variableGroupLineageId, owner.lineageId),
      );
}

const ensureConfigKey = Effect.fn("EnvironmentDesign.ensureConfigKey")(
  function* (input: {
    readonly projectId: string;
    readonly owner: ConfigKeyOwner;
    readonly key: string;
  }) {
    const database = yield* Database;
    const created = yield* database.drizzle
      .insert(configKey)
      .values({
        projectId: input.projectId,
        scope: input.owner.scope,
        serviceLineageId:
          input.owner.scope === "service_lineage" ? input.owner.lineageId : null,
        variableGroupLineageId:
          input.owner.scope === "variable_group_lineage"
            ? input.owner.lineageId
            : null,
        canonicalName: input.key,
      })
      .onConflictDoNothing()
      .returning({ id: configKey.id });
    if (created[0] !== undefined) return created[0].id;
    const existing = yield* database.drizzle
      .select({ id: configKey.id })
      .from(configKey)
      .where(
        and(
          eq(configKey.projectId, input.projectId),
          configKeyOwnerCondition(input.owner),
          eq(configKey.canonicalName, input.key),
        ),
      )
      .limit(1);
    if (existing[0] === undefined) {
      return yield* Effect.die(`PostgreSQL did not return config key ${input.key}.`);
    }
    return existing[0].id;
  },
);

const getLiveConfigVariableId = Effect.fn(
  "EnvironmentDesign.getLiveConfigVariableId",
)(function* (configKeyId: string, environmentId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ variableId: variable.id })
    .from(configValue)
    .innerJoin(configKey, eq(configValue.configKeyId, configKey.id))
    .leftJoin(
      service,
      and(
        eq(configKey.scope, "service_lineage"),
        eq(service.environmentId, environmentId),
        eq(service.lineageId, configKey.serviceLineageId),
      ),
    )
    .leftJoin(
      environmentVariableGroup,
      and(
        eq(configKey.scope, "variable_group_lineage"),
        eq(environmentVariableGroup.environmentId, environmentId),
        eq(environmentVariableGroup.lineageId, configKey.variableGroupLineageId),
      ),
    )
    .leftJoin(
      variable,
      and(
        eq(variable.configKeyId, configValue.configKeyId),
        or(
          eq(variable.serviceId, service.id),
          eq(variable.variableGroupId, environmentVariableGroup.id),
        ),
      ),
    )
    .where(
      and(
        eq(configValue.configKeyId, configKeyId),
        eq(configValue.environmentId, environmentId),
      ),
    )
    .limit(1);
  return rows[0]?.variableId ?? null;
});

const assertConfigKeyAvailable = Effect.fn(
  "EnvironmentDesign.assertConfigKeyAvailable",
)(function* (input: {
  readonly configKeyId: string;
  readonly environmentId: string;
  readonly variableId?: string;
  readonly key: string;
}) {
  const liveVariableId = yield* getLiveConfigVariableId(
    input.configKeyId,
    input.environmentId,
  );
  if (liveVariableId !== null && liveVariableId !== input.variableId) {
    return yield* new Conflict({
      message: `Variable key ${input.key} already has a live value in this environment.`,
    });
  }
});

const preserveConfigPresence = Effect.fn(
  "EnvironmentDesign.preserveConfigPresence",
)(function* (projectId: string, configKeyId: string, environmentId: string) {
  const database = yield* Database;
  yield* database.drizzle
    .insert(configValue)
    .values({ projectId, configKeyId, environmentId })
    .onConflictDoUpdate({
      target: [configValue.configKeyId, configValue.environmentId],
      set: { updatedAt: new Date() },
    });
});

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

const persistCreatedVariable = Effect.fn(
  "EnvironmentDesign.persistCreatedVariable",
)(function* (input: {
  readonly projectId: string;
  readonly environmentId: string;
  readonly configKeyId: string;
  readonly values: VariableWriteValues;
}) {
  const database = yield* Database;
  const { encryptedValue, ...values } = input.values;
  const rows = yield* database.drizzle
    .insert(variable)
    .values({
      ...values,
      organizationId: organizationIdForProject(input.projectId),
      projectId: input.projectId,
      configKeyId: input.configKeyId,
    })
    .returning(variableBaseColumns);
  const row = rows[0];
  if (row === undefined) return null;
  if (encryptedValue !== null) {
    yield* database.drizzle
      .insert(variableSecret)
      .values({ variableId: row.id, encryptedValue });
  }
  yield* preserveConfigPresence(input.projectId, input.configKeyId, input.environmentId);
  return toVariableRecord({ ...row, encryptedValue });
});

export const createOwnedVariable = Effect.fn(
  "EnvironmentDesign.createOwnedVariable",
)(function* (input: {
  readonly projectId: string;
  readonly environmentId: string;
  readonly owner: ConfigKeyOwner;
  readonly values: VariableWriteValues;
  readonly lookupSlug?: LookupSlug;
}) {
  const configKeyId = yield* ensureConfigKey({
    projectId: input.projectId,
    owner: input.owner,
    key: input.values.key,
  });
  yield* assertConfigKeyAvailable({
    configKeyId,
    environmentId: input.environmentId,
    key: input.values.key,
  });
  const record = yield* persistCreatedVariable({
    projectId: input.projectId,
    environmentId: input.environmentId,
    configKeyId,
    values: input.values,
  });
  if (record === null || input.lookupSlug === undefined) return record;
  return yield* getVariableRecord(record.id, input.lookupSlug);
});

export const getVariableRow = Effect.fn("EnvironmentDesign.getVariableRow")(
  function* (input: {
    readonly variableId: string;
    readonly serviceId?: string;
    readonly variableGroupId?: string;
  }) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select(variableColumns)
      .from(variable)
      .leftJoin(variableSecret, eq(variableSecret.variableId, variable.id))
      .where(
        and(
          eq(variable.id, input.variableId),
          input.serviceId === undefined
            ? undefined
            : eq(variable.serviceId, input.serviceId),
          input.variableGroupId === undefined
            ? undefined
            : eq(variable.variableGroupId, input.variableGroupId),
        ),
      )
      .limit(1);
    return rows[0] === undefined ? null : decodeVariableRow(rows[0]);
  },
);

export const listVariableRowsForGroup = Effect.fn(
  "EnvironmentDesign.listVariableRowsForGroup",
)(function* (variableGroupId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select(variableColumns)
    .from(variable)
    .leftJoin(variableSecret, eq(variableSecret.variableId, variable.id))
    .where(eq(variable.variableGroupId, variableGroupId));
  return rows.map(decodeVariableRow);
});

const getVariableRecord = Effect.fn("EnvironmentDesign.getVariableRecord")(
  function* (variableId: string, lookupSlug: LookupSlug = () => null) {
    const row = yield* getVariableRow({ variableId });
    return row === null ? null : toVariableRecord(row, lookupSlug);
  },
);

export const updateOwnedVariable = Effect.fn(
  "EnvironmentDesign.updateOwnedVariable",
)(function* (input: {
  readonly projectId: string;
  readonly environmentId: string;
  readonly row: VariableRow;
  readonly values: Omit<VariableWriteValues, "id" | "serviceId" | "variableGroupId">;
  readonly lookupSlug?: LookupSlug;
}) {
  yield* assertConfigKeyAvailable({
    configKeyId: input.row.configKeyId,
    environmentId: input.environmentId,
    variableId: input.row.id,
    key: input.values.key,
  });
  const database = yield* Database;
  const { encryptedValue, ...values } = input.values;
  const rows = yield* database.drizzle
    .update(variable)
    .set({ ...values, updatedAt: new Date() })
    .where(and(eq(variable.id, input.row.id), eq(variable.projectId, input.projectId)))
    .returning(variableBaseColumns);
  const row = rows[0];
  if (row === undefined) return null;
  if (encryptedValue === null) {
    yield* database.drizzle
      .delete(variableSecret)
      .where(eq(variableSecret.variableId, row.id));
  } else {
    yield* database.drizzle
      .insert(variableSecret)
      .values({ variableId: row.id, encryptedValue })
      .onConflictDoUpdate({
        target: variableSecret.variableId,
        set: { encryptedValue },
      });
  }
  yield* preserveConfigPresence(input.projectId, row.configKeyId, input.environmentId);
  return toVariableRecord(
    { ...row, encryptedValue },
    input.lookupSlug,
  );
});

export const updateVariableMetadata = Effect.fn(
  "EnvironmentDesign.updateVariableMetadata",
)(function* (input: {
  readonly variableId: string;
  readonly serviceId?: string;
  readonly variableGroupId?: string;
  readonly description?: string | null;
  readonly exported: boolean;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .update(variable)
    .set({
      description: input.description,
      exported: input.exported,
      updatedAt: new Date(),
    })
    .where(
      and(
        eq(variable.id, input.variableId),
        input.serviceId === undefined
          ? undefined
          : eq(variable.serviceId, input.serviceId),
        input.variableGroupId === undefined
          ? undefined
          : eq(variable.variableGroupId, input.variableGroupId),
      ),
    )
    .returning(variableBaseColumns);
  const row = rows[0];
  if (row === undefined) return null;
  const secrets = yield* database.drizzle
    .select({ encryptedValue: variableSecret.encryptedValue })
    .from(variableSecret)
    .where(eq(variableSecret.variableId, row.id))
    .limit(1);
  return toVariableRecord({
    ...row,
    encryptedValue: secrets[0]?.encryptedValue ?? null,
  });
});

export const deleteVariableRecord = Effect.fn(
  "EnvironmentDesign.deleteVariableRecord",
)(function* (input: {
  readonly variableId: string;
  readonly serviceId?: string;
  readonly variableGroupId?: string;
}) {
  const existing = yield* getVariableRow(input);
  if (existing === null) return null;
  const database = yield* Database;
  const rows = yield* database.drizzle
    .delete(variable)
    .where(eq(variable.id, existing.id))
    .returning({ id: variable.id });
  return rows[0] ?? null;
});

export const deleteServiceVariables = Effect.fn(
  "EnvironmentDesign.deleteServiceVariables",
)(function* (serviceId: string, variableIds: readonly string[]) {
  if (variableIds.length === 0) return;
  const database = yield* Database;
  yield* database.drizzle
    .delete(variable)
    .where(and(eq(variable.serviceId, serviceId), inArray(variable.id, [...variableIds])));
});

export const attachVariableGroup = Effect.fn(
  "EnvironmentDesign.attachVariableGroup",
)(function* (serviceId: string, group: EnvironmentVariableGroupRecord) {
  const database = yield* Database;
  const current = yield* database.drizzle
    .select({ value: max(serviceVariableGroupAttachment.sortOrder) })
    .from(serviceVariableGroupAttachment)
    .where(eq(serviceVariableGroupAttachment.serviceId, serviceId));
  yield* database.drizzle
    .insert(serviceVariableGroupAttachment)
    .values({
      organizationId: organizationIdForService(serviceId),
      serviceId,
      variableGroupId: group.id,
      sortOrder: (current[0]?.value ?? -1) + 1,
    })
    .onConflictDoNothing();
  return group;
});

export const detachVariableGroup = Effect.fn(
  "EnvironmentDesign.detachVariableGroup",
)(function* (serviceId: string, variableGroupId: string) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(serviceVariableGroupAttachment)
    .where(
      and(
        eq(serviceVariableGroupAttachment.serviceId, serviceId),
        eq(serviceVariableGroupAttachment.variableGroupId, variableGroupId),
      ),
    );
});
