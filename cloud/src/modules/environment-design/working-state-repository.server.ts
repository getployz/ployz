import "@tanstack/react-start/server-only";

import { and, eq, inArray, isNull, or } from "drizzle-orm";
import { Effect, Schema } from "effect";
import {
  variable as schemaVariable,
  variableSecret as schemaVariableSecret,
  service as schemaService,
  serviceRegistryCredential as schemaServiceRegistryCredential,
  environmentResource as schemaEnvironmentResource,
  environmentVariableGroup as schemaEnvironmentVariableGroup,
  serviceVariableGroupAttachment as schemaServiceVariableGroupAttachment,
  serviceVolumeAttachment as schemaServiceVolumeAttachment,
} from "#/modules/environment-design/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import { projectServiceDeploymentConfig } from "#/modules/environment-design/services";
import {
  compileSavedEnvironmentIntent,
  savedEnvironmentIntentSchema,
  savedVariableIntent,
  type SavedEnvironmentIntent,
} from "#/modules/environment-design/saved-intent";
import { strictParseOptions } from "#/modules/environment-design/schema";

export type CurrentEnvironmentSnapshotProjection = {
  nodeSnapshots: ReturnType<
    typeof compileSavedEnvironmentIntent
  >["nodeSnapshots"];
  variableProducers: EnvironmentSnapshotVariableProducer[];
  tombstonedVolumeIds: string[];
  revisionMarkers?: string[];
};

const snapshotVariableColumns = {
  id: schemaVariable.id,
  serviceId: schemaVariable.serviceId,
  variableGroupId: schemaVariable.variableGroupId,
  key: schemaVariable.key,
  description: schemaVariable.description,
  exported: schemaVariable.exported,
  valueKind: schemaVariable.valueKind,
  valueParts: schemaVariable.valueParts,
  encryptedValue: schemaVariableSecret.encryptedValue,
  valueFingerprint: schemaVariable.valueFingerprint,
  updatedAt: schemaVariable.updatedAt,
};

export const loadCurrentEnvironmentState = Effect.fn(
  "EnvironmentDesign.loadCurrentEnvironmentState",
)(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [environment] = yield* drizzle
    .select({ slug: schemaEnvironment.namespace })
    .from(schemaEnvironment)
    .where(eq(schemaEnvironment.id, environmentId));
  if (!environment) return yield* new Conflict({
    message: "Environment snapshot has no Environment.",
  });

  const services = yield* drizzle
    .select({
      id: schemaService.id,
      environmentId: schemaService.environmentId,
      lineageId: schemaService.lineageId,
      slug: schemaService.slug,
      name: schemaService.name,
      source: schemaService.sourceConfig,
      preDeployCommand: schemaService.preDeployCommand,
      startCommand: schemaService.startCommand,
      healthcheck: schemaService.healthcheck,
      restartPolicy: schemaService.restartPolicy,
      maxRetries: schemaService.maxRetries,
      cron: schemaService.cron,
      replicas: schemaService.replicas,
      cpuLimit: schemaService.cpuLimit,
      memLimit: schemaService.memLimit,
      privateDns: schemaService.privateDns,
      routes: schemaService.routes,
      managedHostname: schemaService.managedHostname,
      build: schemaService.build,
      updatedAt: schemaService.updatedAt,
      encryptedRegistryUsername:
        schemaServiceRegistryCredential.encryptedRegistryUsername,
      encryptedRegistrySecret:
        schemaServiceRegistryCredential.encryptedRegistrySecret,
    })
    .from(schemaService)
    .leftJoin(
      schemaServiceRegistryCredential,
      eq(schemaServiceRegistryCredential.serviceId, schemaService.id),
    )
    .where(
      and(
        eq(schemaService.environmentId, environmentId),
        isNull(schemaService.deletedAt),
      ),
    );

  const resources = yield* drizzle
    .select({
      id: schemaEnvironmentResource.id,
      lineageId: schemaEnvironmentResource.lineageId,
      implementationType: schemaEnvironmentResource.implementationType,
      variableGroupId: schemaEnvironmentResource.variableGroupId,
      name: schemaEnvironmentResource.name,
      deletedAt: schemaEnvironmentResource.deletedAt,
      updatedAt: schemaEnvironmentResource.updatedAt,
    })
    .from(schemaEnvironmentResource)
    .where(eq(schemaEnvironmentResource.environmentId, environmentId));
  const variableGroupIds = resources.flatMap((resource) =>
    resource.implementationType === "variable_group" &&
    resource.variableGroupId &&
    resource.deletedAt === null
      ? [resource.variableGroupId]
      : [],
  );
  const serviceIds = services.map((service) => service.id);
  const variables =
    serviceIds.length > 0 || variableGroupIds.length > 0
      ? yield* drizzle
          .select(snapshotVariableColumns)
          .from(schemaVariable)
          .leftJoin(
            schemaVariableSecret,
            eq(schemaVariableSecret.variableId, schemaVariable.id),
          )
          .where(
            or(
              serviceIds.length > 0
                ? inArray(schemaVariable.serviceId, serviceIds)
                : undefined,
              variableGroupIds.length > 0
                ? inArray(schemaVariable.variableGroupId, variableGroupIds)
                : undefined,
            ),
          )
          .orderBy(schemaVariable.key)
      : [];

  const [variableGroups, variableGroupAttachments, volumeAttachments] =
    yield* Effect.all([
      variableGroupIds.length > 0
        ? drizzle
            .select({
              id: schemaEnvironmentVariableGroup.id,
              lineageId: schemaEnvironmentVariableGroup.lineageId,
              slug: schemaEnvironmentVariableGroup.slug,
              updatedAt: schemaEnvironmentVariableGroup.updatedAt,
            })
            .from(schemaEnvironmentVariableGroup)
            .where(
              inArray(schemaEnvironmentVariableGroup.id, variableGroupIds),
            )
        : Effect.succeed([]),
      serviceIds.length > 0
        ? drizzle
            .select({
              serviceId: schemaServiceVariableGroupAttachment.serviceId,
              sortOrder: schemaServiceVariableGroupAttachment.sortOrder,
              variableGroupId:
                schemaServiceVariableGroupAttachment.variableGroupId,
            })
            .from(schemaServiceVariableGroupAttachment)
            .where(
              inArray(
                schemaServiceVariableGroupAttachment.serviceId,
                serviceIds,
              ),
            )
            .orderBy(
              schemaServiceVariableGroupAttachment.sortOrder,
              schemaServiceVariableGroupAttachment.variableGroupId,
            )
        : Effect.succeed([]),
      drizzle
        .select({
          serviceId: schemaServiceVolumeAttachment.serviceId,
          volumeResourceId: schemaServiceVolumeAttachment.volumeResourceId,
          mountPath: schemaServiceVolumeAttachment.mountPath,
        })
        .from(schemaServiceVolumeAttachment)
        .where(eq(schemaServiceVolumeAttachment.environmentId, environmentId)),
    ]);

  const activeVolumes = resources.filter(
    (resource) =>
      resource.implementationType === "volume" && resource.deletedAt === null,
  );
  const activeVolumeIds = new Set(activeVolumes.map((volume) => volume.id));
  const tombstonedVolumeIds = resources.flatMap((resource) =>
    resource.implementationType === "volume" && resource.deletedAt !== null
      ? [resource.id]
      : [],
  );
  const variablesByGroupId = new Map<
    string,
    SavedEnvironmentIntent["variableGroups"][number]["variables"]
  >();
  const variablesByServiceId = new Map<
    string,
    SavedEnvironmentIntent["services"][number]["variables"]
  >();
  for (const variable of variables) {
    const saved = savedVariableIntent(variable);
    if (variable.variableGroupId) {
      const grouped = variablesByGroupId.get(variable.variableGroupId) ?? [];
      grouped.push(saved);
      variablesByGroupId.set(variable.variableGroupId, grouped);
    } else if (variable.serviceId) {
      const grouped = variablesByServiceId.get(variable.serviceId) ?? [];
      grouped.push(saved);
      variablesByServiceId.set(variable.serviceId, grouped);
    }
  }
  const variableGroupById = new Map(
    variableGroups.map((group) => [group.id, group]),
  );
  const intent = yield* Schema.decodeUnknownEffect(savedEnvironmentIntentSchema)(
    {
    version: 1,
    environmentSlug: environment.slug,
    services: [...services]
      .sort((left, right) => left.id.localeCompare(right.id))
      .map((service) => {
        const compiled = projectServiceDeploymentConfig({
          ...service,
          env: {},
          mounts: [],
        });
        const { env: _env, mounts: _mounts, ...config } = compiled;
        void _env;
        void _mounts;
        return {
          id: service.id,
          lineageId: service.lineageId,
          slug: service.slug,
          config,
          variables: variablesByServiceId.get(service.id) ?? [],
          variableGroupAttachments: variableGroupAttachments
            .filter(
              (attachment) =>
                attachment.serviceId === service.id &&
                variableGroupById.has(attachment.variableGroupId),
            )
            .map(({ variableGroupId, sortOrder }) => ({
              variableGroupId,
              sortOrder,
            })),
          volumeAttachments: volumeAttachments
            .filter(
              (attachment) =>
                attachment.serviceId === service.id &&
                activeVolumeIds.has(attachment.volumeResourceId),
            )
            .map(({ volumeResourceId, mountPath }) => ({
              volumeResourceId,
              mountPath,
            }))
            .sort(
              (left, right) =>
                left.volumeResourceId.localeCompare(right.volumeResourceId) ||
                left.mountPath.localeCompare(right.mountPath),
            ),
          encryptedRegistryUsername: service.encryptedRegistryUsername,
          encryptedRegistrySecret: service.encryptedRegistrySecret,
        };
      }),
    variableGroups: [...resources]
      .sort((left, right) => left.id.localeCompare(right.id))
      .flatMap((resource) => {
        if (
          resource.implementationType !== "variable_group" ||
          !resource.variableGroupId ||
          resource.deletedAt !== null
        )
          return [];
        const group = variableGroupById.get(resource.variableGroupId);
        if (!group) return [];
        return [
          {
            resourceId: resource.id,
            resourceLineageId: resource.lineageId,
            variableGroupId: group.id,
            variableGroupLineageId: group.lineageId,
            slug: group.slug,
            name: resource.name,
            variables: variablesByGroupId.get(group.id) ?? [],
          },
        ];
      }),
    volumes: [...activeVolumes]
      .sort((left, right) => left.id.localeCompare(right.id))
      .map((volume) => ({
        resourceId: volume.id,
        resourceLineageId: volume.lineageId,
        name: volume.name,
      })),
    },
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message:
            "Working State contains relationships to nodes that are not being saved.",
        }),
    ),
  );

  const revisionMarkers = [
    ...services.map(
      (service) => `service:${service.id}:${service.updatedAt.toISOString()}`,
    ),
    ...resources.map(
      (resource) =>
        `resource:${resource.id}:${resource.updatedAt.toISOString()}`,
    ),
    ...variableGroups.map(
      (group) => `variable-group:${group.id}:${group.updatedAt.toISOString()}`,
    ),
    ...variables.map(
      (variable) =>
        `variable:${variable.id}:${variable.updatedAt.toISOString()}`,
    ),
  ];

  return {
    intent,
    projection: {
      ...compileSavedEnvironmentIntent({ environmentId, intent }),
      tombstonedVolumeIds,
      revisionMarkers,
    } satisfies CurrentEnvironmentSnapshotProjection,
  };
});

export const loadCurrentEnvironmentSnapshotProjection = Effect.fn(
  "EnvironmentDesign.loadCurrentEnvironmentSnapshotProjection",
)(function* (environmentId: string) {
  return (yield* loadCurrentEnvironmentState(environmentId)).projection;
});
