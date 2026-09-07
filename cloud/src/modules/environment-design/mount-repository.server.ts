import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import {
  environmentResource,
  service,
  serviceVolumeAttachment,
} from "#/modules/environment-design/tables";
import { organizationIdForProject } from "#/db/scope-values.server";
import { Database } from "#/server/database.server";
import { decodeStrict } from "./schema";
import {
  environmentServiceVolumeAttachmentSchema,
  type ServiceMount,
} from "./service-volume-attachments";

export const getMountTarget = Effect.fn("EnvironmentDesign.getMountTarget")(
  function* (serviceId: string, volumeResourceId: string) {
    const database = yield* Database;
    const [services, volumes] = yield* Effect.all(
      [
        database.drizzle
          .select({ environmentId: service.environmentId })
          .from(service)
          .where(eq(service.id, serviceId))
          .limit(1),
        database.drizzle
          .select({
            environmentId: environmentResource.environmentId,
            implementationType: environmentResource.implementationType,
            deletedAt: environmentResource.deletedAt,
          })
          .from(environmentResource)
          .where(eq(environmentResource.id, volumeResourceId))
          .limit(1),
      ],
      { concurrency: "unbounded" },
    );
    return {
      service: services[0] ?? null,
      volume: volumes[0] ?? null,
    };
  },
);

export const listServiceMounts = Effect.fn(
  "EnvironmentDesign.listServiceMounts",
)(function* (serviceId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      volumeResourceId: serviceVolumeAttachment.volumeResourceId,
      mountPath: serviceVolumeAttachment.mountPath,
    })
    .from(serviceVolumeAttachment)
    .where(eq(serviceVolumeAttachment.serviceId, serviceId));
  return rows satisfies ServiceMount[];
});

export const insertServiceMount = Effect.fn(
  "EnvironmentDesign.insertServiceMount",
)(function* (input: {
  readonly projectId: string;
  readonly environmentId: string;
  readonly serviceId: string;
  readonly volumeResourceId: string;
  readonly mountPath: string;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .insert(serviceVolumeAttachment)
    .values({
      organizationId: organizationIdForProject(input.projectId),
      ...input,
    })
    .returning({
      environmentId: serviceVolumeAttachment.environmentId,
      serviceId: serviceVolumeAttachment.serviceId,
      volumeResourceId: serviceVolumeAttachment.volumeResourceId,
      mountPath: serviceVolumeAttachment.mountPath,
    });
  return rows[0] === undefined
    ? null
    : decodeStrict(environmentServiceVolumeAttachmentSchema, rows[0]);
});

export const updateServiceMount = Effect.fn(
  "EnvironmentDesign.updateServiceMount",
)(function* (input: {
  readonly environmentId: string;
  readonly serviceId: string;
  readonly volumeResourceId: string;
  readonly mountPath: string;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .update(serviceVolumeAttachment)
    .set({ mountPath: input.mountPath, updatedAt: new Date() })
    .where(
      and(
        eq(serviceVolumeAttachment.environmentId, input.environmentId),
        eq(serviceVolumeAttachment.serviceId, input.serviceId),
        eq(serviceVolumeAttachment.volumeResourceId, input.volumeResourceId),
      ),
    )
    .returning({
      environmentId: serviceVolumeAttachment.environmentId,
      serviceId: serviceVolumeAttachment.serviceId,
      volumeResourceId: serviceVolumeAttachment.volumeResourceId,
      mountPath: serviceVolumeAttachment.mountPath,
    });
  return rows[0] === undefined
    ? null
    : decodeStrict(environmentServiceVolumeAttachmentSchema, rows[0]);
});

export const deleteServiceMount = Effect.fn(
  "EnvironmentDesign.deleteServiceMount",
)(function* (input: {
  readonly environmentId: string;
  readonly serviceId: string;
  readonly volumeResourceId: string;
}) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(serviceVolumeAttachment)
    .where(
      and(
        eq(serviceVolumeAttachment.environmentId, input.environmentId),
        eq(serviceVolumeAttachment.serviceId, input.serviceId),
        eq(serviceVolumeAttachment.volumeResourceId, input.volumeResourceId),
      ),
    );
  return input;
});
