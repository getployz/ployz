import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import {
  useEnvironmentResourcesCollection,
  useServiceVolumeAttachmentsCollection,
  useServicesCollection,
  useVolumeResourcesCollection,
} from "#/modules/services/services.collection";
import {
  variableGroupResourceRecordSchema,
  volumeResourceRecordSchema,
  type VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import {
  environmentServiceVolumeAttachmentSchema,
  type EnvironmentServiceVolumeAttachment,
} from "#/modules/environment-design/service-volume-attachments";
import type { EnvironmentNodeNameIdentity } from "#/modules/environment-design/environment-node-names";

export type VolumeResourceRouteParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  resourceId: string;
};

export type VolumeDrawerService = { id: string; name: string };

export type VolumeDrawerState = {
  organizationSlug: string;
  resource: VolumeResourceRecord;
  environmentId: string;
  environmentNodes: EnvironmentNodeNameIdentity[];
  services: VolumeDrawerService[];
  /** All mounts in the environment, used to detect per-service path conflicts. */
  attachments: EnvironmentServiceVolumeAttachment[];
};

export function useVolumeDrawerState(
  params: VolumeResourceRouteParams,
): VolumeDrawerState | null {
  const volumeResources = useVolumeResourcesCollection(params.organizationSlug);
  const environmentResources = useEnvironmentResourcesCollection(
    params.organizationSlug,
  );
  const servicesCollection = useServicesCollection(params.organizationSlug);
  const serviceVolumeAttachments = useServiceVolumeAttachmentsCollection(
    params.organizationSlug,
  );
  const { data: volumeRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ resource: volumeResources })
        .where(({ resource }) => eq(resource.projectSlug, params.projectSlug))
        .where(({ resource }) =>
          eq(resource.environmentSlug, params.environmentSlug),
        )
        .select(({ resource }) => resource),
  });
  const { data: variableGroupResourceRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ resource: environmentResources })
        .where(({ resource }) => eq(resource.projectSlug, params.projectSlug))
        .where(({ resource }) =>
          eq(resource.environmentSlug, params.environmentSlug),
        )
        .select(({ resource }) => resource),
  });
  const { data: services } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ service: servicesCollection })
        .where(({ service }) => eq(service.projectSlug, params.projectSlug))
        .where(({ service }) =>
          eq(service.environmentSlug, params.environmentSlug),
        )
        .select(({ service }) => service),
  });
  const { data: attachmentRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ attachment: serviceVolumeAttachments })
        .select(({ attachment }) => attachment),
  });
  const volumes = volumeRows.map((resource) =>
    parseLiveQueryRow(volumeResourceRecordSchema, resource),
  );
  const variableGroupResources = variableGroupResourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );
  const environmentAttachments = attachmentRows.map((attachment) =>
    parseLiveQueryRow(environmentServiceVolumeAttachmentSchema, attachment),
  );

  const resourceRow =
    volumes.find((item) => item.resource.id === params.resourceId) ?? null;

  if (!resourceRow) {
    return null;
  }
  const resource = resourceRow;

  const environmentId = resource.resource.environmentId;
  const attachments = environmentAttachments.flatMap((attachment) =>
    attachment.environmentId === environmentId
      ? [{
          environmentId: attachment.environmentId,
          serviceId: attachment.serviceId,
          volumeResourceId: attachment.volumeResourceId,
          mountPath: attachment.mountPath,
        }]
      : [],
  );

  return {
    organizationSlug: params.organizationSlug,
    resource,
    environmentId,
    services: services.map((service) => ({ id: service.id, name: service.name })),
    attachments,
    environmentNodes: [
      ...services.map((service) => ({
        type: "service" as const,
        id: service.id,
        name: service.name,
      })),
      ...variableGroupResources.map((item) => ({
        type: "variable_group" as const,
        id: item.resource.id,
        name: item.resource.name,
      })),
      ...volumes.map((item) => ({
        type: "volume" as const,
        id: item.resource.id,
        name: item.resource.name,
      })),
    ],
  };
}
