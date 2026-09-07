import { createLiveQueryCollection, eq, toArray, type Collection } from "@tanstack/react-db";
import {
  getCanvasPositionsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getRawEnvironmentResourcesCollection,
  getResourceLineagesCollection,
  getServiceVariableGroupAttachmentsCollection,
  getServiceVolumeAttachmentsCollection,
  getVariableGroupsCollection,
} from "#/electric/collections";
import { plainRowCollection, withoutVirtualProps } from "#/lib/tanstack-db";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import type { VariableRecord } from "#/modules/environment-design/variables";

type ResourceCollectionsInput = {
  organizationSlug: string;
  variables: Collection<VariableRecord>;
};

export function createEnvironmentResourcesCollection(input: ResourceCollectionsInput) {
  const resources = getRawEnvironmentResourcesCollection(input.organizationSlug);
  const lineages = getResourceLineagesCollection(input.organizationSlug);
  const variableGroups = getVariableGroupsCollection(input.organizationSlug);
  const positions = getCanvasPositionsCollection(input.organizationSlug);
  const projects = getProjectsCollection(input.organizationSlug);
  const environments = getEnvironmentsCollection(input.organizationSlug);
  const attachments =
    getServiceVariableGroupAttachmentsCollection(input.organizationSlug);

  const rows = createLiveQueryCollection({
    id: `electric:${input.organizationSlug}:variable-group-resource-relationships`,
    startSync: true,
    query: (q) => q
      .from({ rawVariableGroupResource: resources })
      .where(({ rawVariableGroupResource }) => eq(rawVariableGroupResource.implementationType, "variable_group"))
      .innerJoin({ variableGroupResourceLineage: lineages }, ({ rawVariableGroupResource, variableGroupResourceLineage }) =>
        eq(rawVariableGroupResource.lineageId, variableGroupResourceLineage.id))
      .innerJoin({ resourceVariableGroup: variableGroups }, ({ rawVariableGroupResource, resourceVariableGroup }) =>
        eq(rawVariableGroupResource.variableGroupId, resourceVariableGroup.id))
      .innerJoin({ variableGroupResourceProject: projects }, ({ rawVariableGroupResource, variableGroupResourceProject }) =>
        eq(rawVariableGroupResource.projectId, variableGroupResourceProject.id))
      .innerJoin({ variableGroupResourceEnvironment: environments }, ({ rawVariableGroupResource, variableGroupResourceEnvironment }) =>
        eq(rawVariableGroupResource.environmentId, variableGroupResourceEnvironment.id))
      .select(({ rawVariableGroupResource, variableGroupResourceLineage, resourceVariableGroup, variableGroupResourceProject, variableGroupResourceEnvironment }) => ({
        resource: rawVariableGroupResource,
        lineage: variableGroupResourceLineage,
        variableGroup: resourceVariableGroup,
        projectSlug: variableGroupResourceProject.slug,
        environmentSlug: variableGroupResourceEnvironment.namespace,
        canvasPositions: toArray(
          q.from({ variableGroupResourcePosition: positions })
            .where(({ variableGroupResourcePosition }) => eq(variableGroupResourcePosition.resourceId, rawVariableGroupResource.id))
            .where(({ variableGroupResourcePosition }) => eq(variableGroupResourcePosition.resourceType, "variable_group"))
            .findOne(),
        ),
        variables: toArray(
          q.from({ resourceVariable: input.variables })
            .where(({ resourceVariable }) => eq(resourceVariable.variableGroupId, resourceVariableGroup.id)),
        ),
        consumers: toArray(
          q.from({ variableGroupConsumerAttachment: attachments })
            .where(({ variableGroupConsumerAttachment }) => eq(variableGroupConsumerAttachment.variableGroupId, resourceVariableGroup.id)),
        ),
      })),
  });

  const collection = createLiveQueryCollection({
    id: `electric:${input.organizationSlug}:variable-group-resources`,
    startSync: true,
    query: (q) => q.from({ variableGroupResourceRelationships: rows }).fn.select(({ variableGroupResourceRelationships }) => {
      const variables = variableGroupResourceRelationships.variables.map(withoutVirtualProps);
      const resource = variableGroupResourceRelationships.resource;
      const lineage = variableGroupResourceRelationships.lineage;
      const variableGroup = variableGroupResourceRelationships.variableGroup;
      return {
        resource: {
          id: resource.id,
          projectId: resource.projectId,
          environmentId: resource.environmentId,
          lineageId: resource.lineageId,
          implementationType: "variable_group" as const,
          variableGroupId: variableGroup.id,
          name: resource.name,
          slug: resource.slug,
          deletedAt: resource.deletedAt,
          createdAt: resource.createdAt,
          updatedAt: resource.updatedAt,
        },
        lineage: {
          id: lineage.id,
          projectId: lineage.projectId,
          canonicalName: lineage.canonicalName,
          canonicalSlug: lineage.canonicalSlug,
          createdAt: lineage.createdAt,
          updatedAt: lineage.updatedAt,
        },
        variableGroup: {
          id: variableGroup.id,
          projectId: variableGroup.projectId,
          environmentId: variableGroup.environmentId,
          lineageId: variableGroup.lineageId,
          name: variableGroup.name,
          slug: variableGroup.slug,
          createdAt: variableGroup.createdAt,
          updatedAt: variableGroup.updatedAt,
        },
        canvasPosition: variableGroupResourceRelationships.canvasPositions[0]
          ? withoutVirtualProps(variableGroupResourceRelationships.canvasPositions[0])
          : null,
        variables,
        exports: variables.flatMap((variable) =>
          variable.exported
            ? [{ key: variable.key, value: variable.value, variableId: variable.id }]
            : []),
        consumerCount: variableGroupResourceRelationships.consumers.length,
        projectSlug: variableGroupResourceRelationships.projectSlug,
        environmentSlug: variableGroupResourceRelationships.environmentSlug,
      } satisfies VariableGroupResourceRecord;
    }),
    getKey: (item) => item.resource.id,
  });

  const plain = plainRowCollection(collection);
  // SAFETY: live-query inference keeps nested query shapes; the mapper already satisfies VariableGroupResourceRecord.
  return plain as typeof plain & Collection<VariableGroupResourceRecord>;
}

export function createVolumeResourcesCollection(input: ResourceCollectionsInput) {
  const resources = getRawEnvironmentResourcesCollection(input.organizationSlug);
  const lineages = getResourceLineagesCollection(input.organizationSlug);
  const positions = getCanvasPositionsCollection(input.organizationSlug);
  const projects = getProjectsCollection(input.organizationSlug);
  const environments = getEnvironmentsCollection(input.organizationSlug);
  const attachments = getServiceVolumeAttachmentsCollection(input.organizationSlug);

  const rows = createLiveQueryCollection({
    id: `electric:${input.organizationSlug}:volume-resource-relationships`,
    startSync: true,
    query: (q) => q.from({ rawVolumeResource: resources })
      .where(({ rawVolumeResource }) => eq(rawVolumeResource.implementationType, "volume"))
      .innerJoin({ volumeResourceLineage: lineages }, ({ rawVolumeResource, volumeResourceLineage }) =>
        eq(rawVolumeResource.lineageId, volumeResourceLineage.id))
      .innerJoin({ volumeResourceProject: projects }, ({ rawVolumeResource, volumeResourceProject }) =>
        eq(rawVolumeResource.projectId, volumeResourceProject.id))
      .innerJoin({ volumeResourceEnvironment: environments }, ({ rawVolumeResource, volumeResourceEnvironment }) =>
        eq(rawVolumeResource.environmentId, volumeResourceEnvironment.id))
      .select(({ rawVolumeResource, volumeResourceLineage, volumeResourceProject, volumeResourceEnvironment }) => ({
        resource: rawVolumeResource,
        lineage: volumeResourceLineage,
        projectSlug: volumeResourceProject.slug,
        environmentSlug: volumeResourceEnvironment.namespace,
        canvasPositions: toArray(
          q.from({ volumeResourcePosition: positions })
            .where(({ volumeResourcePosition }) => eq(volumeResourcePosition.resourceId, rawVolumeResource.id))
            .where(({ volumeResourcePosition }) => eq(volumeResourcePosition.resourceType, "volume"))
            .findOne(),
        ),
        attachments: toArray(
          q.from({ volumeConsumerAttachment: attachments })
            .where(({ volumeConsumerAttachment }) => eq(volumeConsumerAttachment.volumeResourceId, rawVolumeResource.id)),
        ),
      })),
  });

  const collection = createLiveQueryCollection({
    id: `electric:${input.organizationSlug}:volume-resources`,
    startSync: true,
    query: (q) => q.from({ volumeResourceRelationships: rows }).fn.select(({ volumeResourceRelationships }) => {
      const resource = volumeResourceRelationships.resource;
      const lineage = volumeResourceRelationships.lineage;
      return {
        resource: {
          id: resource.id,
          projectId: resource.projectId,
          environmentId: resource.environmentId,
          lineageId: resource.lineageId,
          implementationType: "volume" as const,
          variableGroupId: null,
          name: resource.name,
          slug: resource.slug,
          deletedAt: resource.deletedAt,
          createdAt: resource.createdAt,
          updatedAt: resource.updatedAt,
        },
        lineage: {
          id: lineage.id,
          projectId: lineage.projectId,
          canonicalName: lineage.canonicalName,
          canonicalSlug: lineage.canonicalSlug,
          createdAt: lineage.createdAt,
          updatedAt: lineage.updatedAt,
        },
        canvasPosition: volumeResourceRelationships.canvasPositions[0]
          ? withoutVirtualProps(volumeResourceRelationships.canvasPositions[0])
          : null,
        attachments: volumeResourceRelationships.attachments.map((attachment) => ({
          serviceId: attachment.serviceId,
          mountPath: attachment.mountPath,
        })),
        consumerCount: volumeResourceRelationships.attachments.length,
        runtimeStatus: null,
        projectSlug: volumeResourceRelationships.projectSlug,
        environmentSlug: volumeResourceRelationships.environmentSlug,
      } satisfies VolumeResourceRecord;
    }),
    getKey: (item) => item.resource.id,
  });

  const plain = plainRowCollection(collection);
  // SAFETY: live-query inference keeps nested query shapes; the mapper already satisfies VolumeResourceRecord.
  return plain as typeof plain & Collection<VolumeResourceRecord>;
}
