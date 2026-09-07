import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import {
  environmentCanvasNodePosition,
  service,
  serviceLineage,
  serviceRegistryCredential,
} from "#/modules/environment-design/tables";
import { environment, project } from "#/modules/project/tables";
import {
  organizationIdForEnvironment,
  organizationIdForProject,
} from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import {
  SecretEncryption,
  type SecretEncryptionService,
} from "#/utils/encrypted-secret.server";
import { decodeStrict } from "./schema";
import {
  serviceCanvasPositionSelectSchema,
  serviceSelectSchema,
  serviceWithContextSelectSchema,
  type ServiceCanvasPositionRecord,
  type ServiceRecord,
  type ServiceSource,
  type ServiceWithContextRecord,
  type UpdateServiceInput,
} from "./services";
import { encryptedSecretValueSchema } from "./variables";

const serviceBaseColumns = {
  id: service.id,
  environmentId: service.environmentId,
  lineageId: service.lineageId,
  name: service.name,
  slug: service.slug,
  source: service.sourceConfig,
  hasRegistryCredential: service.hasRegistryCredential,
  preDeployCommand: service.preDeployCommand,
  startCommand: service.startCommand,
  healthcheck: service.healthcheck,
  restartPolicy: service.restartPolicy,
  maxRetries: service.maxRetries,
  cron: service.cron,
  replicas: service.replicas,
  cpuLimit: service.cpuLimit,
  memLimit: service.memLimit,
  privateDns: service.privateDns,
  routes: service.routes,
  managedHostname: service.managedHostname,
  build: service.build,
  firstDeployedAt: service.firstDeployedAt,
  deletedAt: service.deletedAt,
  createdAt: service.createdAt,
  updatedAt: service.updatedAt,
};

const serviceCredentialColumns = {
  encryptedRegistryUsername: serviceRegistryCredential.encryptedRegistryUsername,
  encryptedRegistrySecret: serviceRegistryCredential.encryptedRegistrySecret,
};

const serviceColumns = { ...serviceBaseColumns, ...serviceCredentialColumns };

const canvasPositionColumns = {
  id: environmentCanvasNodePosition.id,
  environmentId: environmentCanvasNodePosition.environmentId,
  resourceType: environmentCanvasNodePosition.resourceType,
  resourceId: environmentCanvasNodePosition.resourceId,
  x: environmentCanvasNodePosition.x,
  y: environmentCanvasNodePosition.y,
  createdAt: environmentCanvasNodePosition.createdAt,
  updatedAt: environmentCanvasNodePosition.updatedAt,
};

type CanvasPositionRow = {
  readonly id: string;
  readonly environmentId: string;
  readonly resourceType: string;
  readonly resourceId: string;
  readonly x: number;
  readonly y: number;
  readonly createdAt: Date;
  readonly updatedAt: Date;
};

const serviceWithContextColumns = {
  ...serviceColumns,
  projectSlug: project.slug,
  environmentSlug: environment.namespace,
};

export interface StoredServiceCredential {
  readonly id: string;
  readonly source: ServiceSource;
  readonly encryptedRegistryUsername: EncryptedSecretValue | null;
  readonly encryptedRegistrySecret: EncryptedSecretValue | null;
  readonly hasRegistryCredential: boolean;
}

function decodeEncryptedSecret(value: EncryptedSecretValue | null) {
  return value === null
    ? null
    : decodeStrict(encryptedSecretValueSchema, value);
}

export function exposedRegistryCredentialUsername(row: {
  readonly encryptedRegistryUsername: EncryptedSecretValue | null;
  readonly encryptedRegistrySecret: EncryptedSecretValue | null;
}, encryption: SecretEncryptionService) {
  const username = decodeEncryptedSecret(row.encryptedRegistryUsername);
  const secret = decodeEncryptedSecret(row.encryptedRegistrySecret);
  return username === null || secret === null
    ? null
    : encryption.decrypt(username);
}

function toServiceRecord(encryption: SecretEncryptionService, row: typeof serviceColumns extends infer _
  ? {
      id: string;
      environmentId: string;
      lineageId: string;
      name: string;
      slug: string;
      source: ServiceSource;
      encryptedRegistryUsername: EncryptedSecretValue | null;
      encryptedRegistrySecret: EncryptedSecretValue | null;
      hasRegistryCredential: boolean;
      preDeployCommand: string | null;
      startCommand: string | null;
      healthcheck: ServiceRecord["healthcheck"];
      restartPolicy: ServiceRecord["restartPolicy"];
      maxRetries: number;
      cron: string | null;
      replicas: number;
      cpuLimit: number | null;
      memLimit: number | null;
      privateDns: string;
      routes: ServiceRecord["routes"];
      managedHostname: ServiceRecord["managedHostname"];
      build: ServiceRecord["build"];
      firstDeployedAt: Date | null;
      deletedAt: Date | null;
      createdAt: Date;
      updatedAt: Date;
    }
  : never): ServiceRecord {
  return decodeStrict(serviceSelectSchema, {
    id: row.id,
    environmentId: row.environmentId,
    lineageId: row.lineageId,
    name: row.name,
    slug: row.slug,
    source: row.source,
    registryCredentialUsername: exposedRegistryCredentialUsername(row, encryption),
    hasStoredRegistryCredential: row.hasRegistryCredential,
    preDeployCommand: row.preDeployCommand,
    startCommand: row.startCommand,
    healthcheck: row.healthcheck,
    restartPolicy: row.restartPolicy,
    maxRetries: row.maxRetries,
    cron: row.cron,
    replicas: row.replicas,
    cpuLimit: row.cpuLimit,
    memLimit: row.memLimit,
    privateDns: row.privateDns,
    routes: row.routes,
    managedHostname: row.managedHostname,
    build: row.build,
    firstDeployedAt: row.firstDeployedAt,
    deletedAt: row.deletedAt,
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
  });
}

function toServiceWithContextRecord(
  encryption: SecretEncryptionService,
  row: Parameters<typeof toServiceRecord>[1] & {
    readonly projectSlug: string;
    readonly environmentSlug: string;
  },
): ServiceWithContextRecord {
  return decodeStrict(serviceWithContextSelectSchema, {
    ...toServiceRecord(encryption, row),
    projectSlug: row.projectSlug,
    environmentSlug: row.environmentSlug,
  });
}

function toCanvasPosition(row: CanvasPositionRow): ServiceCanvasPositionRecord {
  return decodeStrict(serviceCanvasPositionSelectSchema, row);
}

export const listServicesForEnvironment = Effect.fn(
  "EnvironmentDesign.listServicesForEnvironment",
)(function* (
  environmentId: string,
  context: { readonly projectSlug: string; readonly environmentSlug: string },
) {
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const rows = yield* database.drizzle
    .select({ service: serviceColumns, canvasPosition: canvasPositionColumns })
    .from(service)
    .leftJoin(
      serviceRegistryCredential,
      eq(serviceRegistryCredential.serviceId, service.id),
    )
    .leftJoin(
      environmentCanvasNodePosition,
      and(
        eq(environmentCanvasNodePosition.resourceType, "service"),
        eq(environmentCanvasNodePosition.resourceId, service.id),
      ),
    )
    .where(and(eq(service.environmentId, environmentId), isNull(service.deletedAt)));
  return rows.map((row) => ({
    service: decodeStrict(serviceWithContextSelectSchema, {
      ...toServiceRecord(encryption, row.service),
      ...context,
    }),
    canvasPosition:
      row.canvasPosition === null ? null : toCanvasPosition(row.canvasPosition),
  }));
});

export const getServiceForOrganizationById = Effect.fn(
  "EnvironmentDesign.getServiceForOrganizationById",
)(function* (organizationId: string, serviceId: string) {
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const rows = yield* database.drizzle
    .select({ service: serviceWithContextColumns, canvasPosition: canvasPositionColumns })
    .from(service)
    .leftJoin(
      serviceRegistryCredential,
      eq(serviceRegistryCredential.serviceId, service.id),
    )
    .innerJoin(environment, eq(service.environmentId, environment.id))
    .innerJoin(project, eq(environment.projectId, project.id))
    .leftJoin(
      environmentCanvasNodePosition,
      and(
        eq(environmentCanvasNodePosition.resourceType, "service"),
        eq(environmentCanvasNodePosition.resourceId, service.id),
      ),
    )
    .where(and(eq(project.organizationId, organizationId), eq(service.id, serviceId)))
    .limit(1);
  const row = rows[0];
  return row === undefined
    ? null
    : {
        service: toServiceWithContextRecord(encryption, row.service),
        canvasPosition:
          row.canvasPosition === null ? null : toCanvasPosition(row.canvasPosition),
      };
});

export const getStoredServiceCredential = Effect.fn(
  "EnvironmentDesign.getStoredServiceCredential",
)(function* (environmentId: string, serviceId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      id: service.id,
      source: service.sourceConfig,
      ...serviceCredentialColumns,
      hasRegistryCredential: service.hasRegistryCredential,
    })
    .from(service)
    .leftJoin(
      serviceRegistryCredential,
      eq(serviceRegistryCredential.serviceId, service.id),
    )
    .where(and(eq(service.environmentId, environmentId), eq(service.id, serviceId)))
    .limit(1);
  const row = rows[0];
  if (row === undefined) return null;
  return {
    ...row,
    source: decodeStrict(serviceSelectSchema.fields.source, row.source),
    encryptedRegistryUsername: decodeEncryptedSecret(row.encryptedRegistryUsername),
    encryptedRegistrySecret: decodeEncryptedSecret(row.encryptedRegistrySecret),
  } satisfies StoredServiceCredential;
});

export const setStoredServiceCredential = Effect.fn(
  "EnvironmentDesign.setStoredServiceCredential",
)(function* (input: {
  readonly serviceId: string;
  readonly source: ServiceSource;
  readonly encryptedRegistryUsername: EncryptedSecretValue | null;
  readonly encryptedRegistrySecret: EncryptedSecretValue;
}) {
  const database = yield* Database;
  const updated = yield* database.drizzle
    .update(service)
    .set({
      sourceConfig: input.source,
      hasRegistryCredential: true,
      updatedAt: new Date(),
    })
    .where(eq(service.id, input.serviceId))
    .returning({ id: service.id });
  if (updated[0] === undefined) return null;
  yield* database.drizzle
    .insert(serviceRegistryCredential)
    .values({
      serviceId: input.serviceId,
      encryptedRegistryUsername: input.encryptedRegistryUsername,
      encryptedRegistrySecret: input.encryptedRegistrySecret,
    })
    .onConflictDoUpdate({
      target: serviceRegistryCredential.serviceId,
      set: {
        encryptedRegistryUsername: input.encryptedRegistryUsername,
        encryptedRegistrySecret: input.encryptedRegistrySecret,
      },
    });
  return updated[0];
});

export const updateServiceSource = Effect.fn("EnvironmentDesign.updateServiceSource")(
  function* (serviceId: string, source: ServiceSource) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .update(service)
      .set({ sourceConfig: source, updatedAt: new Date() })
      .where(eq(service.id, serviceId))
      .returning({ id: service.id });
    return rows[0] ?? null;
  },
);

export const insertService = Effect.fn("EnvironmentDesign.insertService")(
  function* (input: {
    readonly projectId: string;
    readonly environmentId: string;
    readonly lineageId: string;
    readonly name: string;
    readonly slug: string;
    readonly source: ServiceSource;
    readonly preDeployCommand: string | null;
    readonly startCommand: string | null;
    readonly healthcheck: ServiceRecord["healthcheck"];
    readonly restartPolicy: ServiceRecord["restartPolicy"];
  }) {
    const database = yield* Database;
    const encryption = yield* SecretEncryption;
    const rows = yield* database.drizzle
      .insert(service)
      .values({
        organizationId: organizationIdForProject(input.projectId),
        projectId: input.projectId,
        environmentId: input.environmentId,
        lineageId: input.lineageId,
        name: input.name,
        slug: input.slug,
        privateDns: input.slug,
        sourceType: input.source.type,
        sourceConfig: input.source,
        preDeployCommand: input.preDeployCommand,
        startCommand: input.startCommand,
        healthcheck: input.healthcheck,
        restartPolicy: input.restartPolicy,
      })
      .onConflictDoNothing()
      .returning(serviceBaseColumns);
    const row = rows[0];
    return row === undefined
      ? null
      : toServiceRecord(encryption, {
          ...row,
          encryptedRegistryUsername: null,
          encryptedRegistrySecret: null,
        });
  },
);

export const deleteServiceLineage = Effect.fn(
  "EnvironmentDesign.deleteServiceLineage",
)(function* (lineageId: string) {
  const database = yield* Database;
  yield* database.drizzle.delete(serviceLineage).where(eq(serviceLineage.id, lineageId));
});

export const insertCanvasPosition = Effect.fn(
  "EnvironmentDesign.insertCanvasPosition",
)(function* (input: {
  readonly environmentId: string;
  readonly resourceId: string;
  readonly x: number;
  readonly y: number;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .insert(environmentCanvasNodePosition)
    .values({
      organizationId: organizationIdForEnvironment(input.environmentId),
      environmentId: input.environmentId,
      resourceType: "service",
      resourceId: input.resourceId,
      x: Math.round(input.x),
      y: Math.round(input.y),
    })
    .returning(canvasPositionColumns);
  return rows[0] === undefined ? null : toCanvasPosition(rows[0]);
});

export const getServiceForUpdate = Effect.fn(
  "EnvironmentDesign.getServiceForUpdate",
)(function* (environmentId: string, serviceId: string) {
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const rows = yield* database.drizzle
    .select(serviceColumns)
    .from(service)
    .leftJoin(
      serviceRegistryCredential,
      eq(serviceRegistryCredential.serviceId, service.id),
    )
    .where(and(eq(service.environmentId, environmentId), eq(service.id, serviceId)))
    .limit(1);
  const row = rows[0];
  return row === undefined
    ? null
    : {
        record: toServiceRecord(encryption, row),
        encryptedRegistryUsername: decodeEncryptedSecret(row.encryptedRegistryUsername),
        encryptedRegistrySecret: decodeEncryptedSecret(row.encryptedRegistrySecret),
      };
});

export const updateServiceRecord = Effect.fn(
  "EnvironmentDesign.updateServiceRecord",
)(function* (
  serviceId: string,
  input: UpdateServiceInput & { readonly name: string; readonly source: ServiceSource },
  credentials: {
    readonly encryptedRegistryUsername: EncryptedSecretValue | null;
    readonly encryptedRegistrySecret: EncryptedSecretValue | null;
  },
) {
  const database = yield* Database;
  const encryption = yield* SecretEncryption;
  const rows = yield* database.drizzle
    .update(service)
    .set({
      name: input.name,
      sourceType: input.source.type,
      sourceConfig: input.source,
      preDeployCommand: input.preDeployCommand,
      startCommand: input.startCommand,
      healthcheck: input.healthcheck,
      restartPolicy: input.restartPolicy,
      maxRetries: input.maxRetries,
      cron: input.cron,
      replicas: input.replicas,
      cpuLimit: input.cpuLimit,
      memLimit: input.memLimit,
      privateDns: input.privateDns,
      routes: input.routes,
      managedHostname: input.managedHostname,
      build: input.build,
      deletedAt: input.deletedAt,
      updatedAt: new Date(),
    })
    .where(eq(service.id, serviceId))
    .returning(serviceBaseColumns);
  return rows[0] === undefined
    ? null
    : toServiceRecord(encryption, { ...rows[0], ...credentials });
});

export const deleteServiceRecords = Effect.fn(
  "EnvironmentDesign.deleteServiceRecords",
)(function* (environmentId: string, serviceIds: readonly string[]) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .delete(service)
    .where(
      and(
        eq(service.environmentId, environmentId),
        inArray(service.id, [...serviceIds]),
      ),
    )
    .returning({ id: service.id });
  return { deletedIds: rows.map((row) => row.id) };
});

export const serviceExists = Effect.fn("EnvironmentDesign.serviceExists")(
  function* (environmentId: string, serviceId: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({ id: service.id })
      .from(service)
      .where(and(eq(service.environmentId, environmentId), eq(service.id, serviceId)))
      .limit(1);
    return rows[0] !== undefined;
  },
);

export const upsertCanvasPosition = Effect.fn(
  "EnvironmentDesign.upsertCanvasPosition",
)(function* (input: {
  readonly environmentId: string;
  readonly serviceId: string;
  readonly x: number;
  readonly y: number;
}) {
  const database = yield* Database;
  const now = new Date();
  const rows = yield* database.drizzle
    .insert(environmentCanvasNodePosition)
    .values({
      organizationId: organizationIdForEnvironment(input.environmentId),
      environmentId: input.environmentId,
      resourceType: "service",
      resourceId: input.serviceId,
      x: Math.round(input.x),
      y: Math.round(input.y),
      updatedAt: now,
    })
    .onConflictDoUpdate({
      target: [
        environmentCanvasNodePosition.environmentId,
        environmentCanvasNodePosition.resourceType,
        environmentCanvasNodePosition.resourceId,
      ],
      set: { x: Math.round(input.x), y: Math.round(input.y), updatedAt: now },
    })
    .returning(canvasPositionColumns);
  return rows[0] === undefined ? null : toCanvasPosition(rows[0]);
});
