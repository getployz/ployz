import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import { environmentCanvasNodePosition, service, serviceRegistryCredential } from "./tables";
import { environment, project } from "#/modules/project/tables";
import { organizationIdForEnvironment, organizationIdForProject } from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import { SecretEncryption, type SecretEncryptionService } from "#/utils/encrypted-secret.server";
import { decodeStrict } from "./schema";
import { serviceCanvasPositionSelectSchema, type ServiceCanvasPositionRecord } from "./services";
import { loadEnvironmentDocument } from "./working-state-repository.server";
import { serviceDocumentRecord } from "./service-document";
export { serviceDocumentRecord } from "./service-document";

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
const toCanvasPosition = (row: ServiceCanvasPositionRecord) => decodeStrict(serviceCanvasPositionSelectSchema, row);

export function exposedRegistryCredentialUsername(row: {
  readonly encryptedRegistryUsername: EncryptedSecretValue | null;
  readonly encryptedRegistrySecret: EncryptedSecretValue | null;
}, encryption: SecretEncryptionService) {
  return row.encryptedRegistryUsername && row.encryptedRegistrySecret
    ? encryption.decrypt(row.encryptedRegistryUsername) : null;
}

export const listServicesForEnvironment = Effect.fn("EnvironmentDesign.listServicesForEnvironment")(
  function* (environmentId: string, context: { readonly projectSlug: string; readonly environmentSlug: string }) {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    const document = yield* loadEnvironmentDocument(environmentId);
    const rows = yield* drizzle.select({ identity: service, credential: serviceRegistryCredential, canvasPosition: canvasPositionColumns })
      .from(service).leftJoin(serviceRegistryCredential, eq(serviceRegistryCredential.serviceId, service.id))
      .leftJoin(environmentCanvasNodePosition, and(eq(environmentCanvasNodePosition.environmentId, environmentId), eq(environmentCanvasNodePosition.resourceType, "service"), eq(environmentCanvasNodePosition.resourceId, service.id)))
      .where(eq(service.environmentId, environmentId));
    return rows.flatMap((row) => {
      const node = document.intent.services.find((node) => node.id === row.identity.id);
      return node ? [{ service: { ...serviceDocumentRecord(row.identity, node, row.credential ? exposedRegistryCredentialUsername(row.credential, encryption) : null), ...context }, canvasPosition: row.canvasPosition ? toCanvasPosition(row.canvasPosition) : null }] : [];
    });
  },
);

export const getServiceForOrganizationById = Effect.fn("EnvironmentDesign.getServiceForOrganizationById")(
  function* (organizationId: string, serviceId: string) {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({ environmentId: environment.id, projectSlug: project.slug, environmentSlug: environment.namespace })
      .from(service).innerJoin(environment, eq(environment.id, service.environmentId)).innerJoin(project, eq(project.id, environment.projectId))
      .where(and(eq(service.id, serviceId), eq(project.organizationId, organizationId)));
    if (!row) return null;
    return (yield* listServicesForEnvironment(row.environmentId, row)).find((value) => value.service.id === serviceId) ?? null;
  },
);

export const getStoredServiceCredential = Effect.fn("EnvironmentDesign.getStoredServiceCredential")(
  function* (environmentId: string, serviceId: string) {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({ identity: service, credential: serviceRegistryCredential }).from(service)
      .leftJoin(serviceRegistryCredential, eq(serviceRegistryCredential.serviceId, service.id))
      .where(and(eq(service.environmentId, environmentId), eq(service.id, serviceId)));
    return row ? { id: row.identity.id, hasRegistryCredential: row.identity.hasRegistryCredential,
      encryptedRegistryUsername: row.credential?.encryptedRegistryUsername ?? null,
      encryptedRegistrySecret: row.credential?.encryptedRegistrySecret ?? null } : null;
  },
);

export const insertServiceIdentity = Effect.fn("EnvironmentDesign.insertServiceIdentity")(
  function* (input: { projectId: string; environmentId: string; lineageId: string }) {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(service).values({ ...input, organizationId: organizationIdForProject(input.projectId) }).returning();
    if (!row) return yield* Effect.die("PostgreSQL did not return the service identity.");
    return row;
  },
);

export const serviceExists = Effect.fn("EnvironmentDesign.serviceExists")(
  function* (environmentId: string, serviceId: string) {
    return (yield* loadEnvironmentDocument(environmentId)).intent.services.some((node) => node.id === serviceId);
  },
);

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
