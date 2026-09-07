import { decodeStrict } from "#/modules/environment-design/schema";
import {
  createOptimisticAction,
  createLiveQueryCollection,
  eq,
  toArray,
  type ExtractContext,
  type GetResult,
  type InitialQueryBuilder,
} from "@tanstack/react-db";
import { toast } from "sonner";
import {
  getCanvasPositionsCollection,
  getEnvironmentsCollection,
  getProjectsCollection,
  getRawServicesCollection,
  getServiceVariableGroupAttachmentsCollection,
  getServiceVolumeAttachmentsCollection,
  getVariableGroupsCollection,
} from "#/electric/collections";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import { getOrganizationDeploymentsCollection } from "#/modules/deployments/deployment-collection";
import {
  createEnvironmentResourcesCollection,
  createVolumeResourcesCollection,
} from "#/modules/environment-design/resource-collections";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import {
  type EnvironmentServiceVolumeAttachment,
  getServiceMountsByServiceId,
} from "#/modules/environment-design/service-volume-attachments";
import { getDeployEnvFromServiceVariables } from "#/modules/deployments/deploy-environment";
import { updateServiceServerFn } from "#/modules/environment-design/service-functions";
import {
  type ServiceDeploymentFieldSelection,
  type ServiceDeployEnv,
  type ServiceDeployMount,
  serviceHealthcheckSchema,
  serviceRestartPolicySchema,
  serviceSourceSchema,
  type ServiceSource,
  type ServiceCanvasPositionRecord,
  type ServiceWithContextRecord,
} from "#/modules/environment-design/services";
import {
  organizationVariablesCollectionOptions,
} from "#/modules/environment-design/variable-collections";
import {
  type EnvironmentServiceVariableGroupAttachment,
  variableSelectSchema,
  type VariableRecord,
} from "#/modules/environment-design/variables";

export type EnvironmentParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

function createServicesCollection(organizationSlug: string) {
  const rawServices = getRawServicesCollection(organizationSlug);
  const projects = getProjectsCollection(organizationSlug);
  const environments = getEnvironmentsCollection(organizationSlug);

  return createLiveQueryCollection({
      id: `electric:${organizationSlug}:services-with-context`,
      startSync: true,
      query: (q) => q
        .from({ rawService: rawServices })
        .innerJoin({ serviceProject: projects }, ({ rawService, serviceProject }) =>
          eq(rawService.projectId, serviceProject.id))
        .innerJoin({ serviceEnvironment: environments }, ({ rawService, serviceEnvironment }) =>
          eq(rawService.environmentId, serviceEnvironment.id))
        .fn.select(({ rawService, serviceProject, serviceEnvironment }) => ({
          id: rawService.id,
          environmentId: rawService.environmentId,
          lineageId: rawService.lineageId,
          name: rawService.name,
          slug: rawService.slug,
          source: rawService.sourceConfig,
          // SAFETY: this projection never decrypts registry usernames; keep the field typed string | null.
          registryCredentialUsername: null as string | null,
          hasStoredRegistryCredential: rawService.hasRegistryCredential,
          preDeployCommand: rawService.preDeployCommand,
          startCommand: rawService.startCommand,
          healthcheck: rawService.healthcheck,
          restartPolicy: rawService.restartPolicy,
          maxRetries: rawService.maxRetries,
          cron: rawService.cron,
          replicas: rawService.replicas,
          cpuLimit: rawService.cpuLimit,
          memLimit: rawService.memLimit,
          privateDns: rawService.privateDns,
          routes: rawService.routes,
          managedHostname: rawService.managedHostname,
          build: rawService.build,
          firstDeployedAt: rawService.firstDeployedAt,
          deletedAt: rawService.deletedAt,
          createdAt: rawService.createdAt,
          updatedAt: rawService.updatedAt,
          projectSlug: serviceProject.slug,
          environmentSlug: serviceEnvironment.namespace,
        })),
      getKey: (item) => item.id,
    });
}

export type ServiceWriter = {
  update(
    serviceId: string,
    updater: (draft: ServiceWithContextRecord) => void,
  ): { isPersisted: { promise: Promise<unknown> } };
};

function createServiceWriter(
  organizationSlug: string,
  services: ReturnType<typeof createServicesCollection>,
): ServiceWriter {
  const rawServices = getRawServicesCollection(organizationSlug);
  const persist = createOptimisticAction<ServiceWithContextRecord>({
    onMutate: (modified) => {
      services.update(modified.id, (draft) => Object.assign(draft, modified));
    },
    mutationFn: async (modified) => {
      try {
        const editableConfig: ServiceDeploymentFieldSelection = {
          name: modified.name,
          source: modified.source,
          preDeployCommand: modified.preDeployCommand,
          startCommand: modified.startCommand,
          healthcheck: modified.healthcheck,
          restartPolicy: modified.restartPolicy,
          maxRetries: modified.maxRetries,
          cron: modified.cron,
          replicas: modified.replicas,
          cpuLimit: modified.cpuLimit,
          memLimit: modified.memLimit,
          privateDns: modified.privateDns,
          routes: modified.routes,
          managedHostname: modified.managedHostname,
          build: modified.build,
          deletedAt: modified.deletedAt ?? null,
        };
        const receipt = await updateServiceServerFn({
          data: {
            organizationSlug,
            environmentId: modified.environmentId,
            serviceId: modified.id,
            ...editableConfig,
            deletedAt: modified.deletedAt ?? null,
          },
        });
        await rawServices.utils.awaitTxId(receipt.txid);
      } catch (error) {
        toast.error(
          error instanceof Error
            ? error.message
            : "Something went wrong while saving this field.",
        );
        throw error;
      }
    },
  });

  return {
    update(serviceId, updater) {
      const current = services.get(serviceId);
      if (!current) throw new Error("Service is not loaded.");
      const modified = structuredClone(current);
      updater(modified);
      return persist(modified);
    },
  };
}

export function getCanvasPositionCollectionKey(
  item: Pick<ServiceCanvasPositionRecord, "resourceType" | "resourceId">,
) {
  return `${item.resourceType}:${item.resourceId}`;
}

function cachedByOrganization<T>(create: (organizationSlug: string) => T) {
  const cache = new Map<string, T>();

  return (organizationSlug: string): T => {
    const existing = cache.get(organizationSlug);
    if (existing) return existing;

    const value = create(organizationSlug);
    cache.set(organizationSlug, value);
    return value;
  };
}

const getServicesCollection = cachedByOrganization(createServicesCollection);

const getServiceWriter = cachedByOrganization((organizationSlug) =>
  createServiceWriter(
    organizationSlug,
    getServicesCollection(organizationSlug),
  ),
);

const getVariablesStore = cachedByOrganization((organizationSlug) =>
  organizationVariablesCollectionOptions({
    organizationSlug,
    getServiceEnvironmentId: (serviceId) =>
      getServicesCollection(organizationSlug).get(serviceId)?.environmentId,
    getVariableGroupEnvironmentId: (variableGroupId) => {
      const variableGroup = Array.from(
        getVariableGroupsCollection(organizationSlug).values(),
      ).find((item) => item.id === variableGroupId);
      return variableGroup?.environmentId;
    },
  }),
);

const getEnvironmentResourcesCollection = cachedByOrganization(
  (organizationSlug) =>
    createEnvironmentResourcesCollection({
      organizationSlug,
      variables: getVariablesStore(organizationSlug).collection,
    }),
);

const getVolumeResourcesCollection = cachedByOrganization(
  (organizationSlug) =>
    createVolumeResourcesCollection({
      organizationSlug,
      variables: getVariablesStore(organizationSlug).collection,
    }),
);

export type ServicesCollection = ReturnType<typeof getServicesCollection>;
export type VariablesCollection = ReturnType<
  typeof getVariablesStore
>["collection"];

export const useServicesCollection = getServicesCollection;
export const useServiceWriter = getServiceWriter;
export const useEnvironmentResourcesCollection =
  getEnvironmentResourcesCollection;
export const useVolumeResourcesCollection = getVolumeResourcesCollection;
export const useServiceVolumeAttachmentsCollection =
  getServiceVolumeAttachmentsCollection;
export const useCanvasPositionsCollection = getCanvasPositionsCollection;
export const useDeploymentsCollection = getOrganizationDeploymentsCollection;
export const useVariablesCollection = (organizationSlug: string) =>
  getVariablesStore(organizationSlug).collection;
export const useVariableWriter = (organizationSlug: string) =>
  getVariablesStore(organizationSlug).writer;
export const useServiceVariableGroupAttachmentsCollection =
  getServiceVariableGroupAttachmentsCollection;

export function buildEnvironmentServicesViewQuery(
  q: InitialQueryBuilder,
  params: EnvironmentParams,
  collections: {
    services: ServicesCollection;
    canvasPositions: ReturnType<typeof getCanvasPositionsCollection>;
    variables: VariablesCollection;
  },
) {
  return q
    .from({ service: collections.services })
    .where(({ service }) => eq(service.projectSlug, params.projectSlug))
    .where(({ service }) => eq(service.environmentSlug, params.environmentSlug))
    .select(({ service }) => ({
      service,
      canvasPositions: toArray(
        q
          .from({ canvasPosition: collections.canvasPositions })
          .where(({ canvasPosition }) =>
            eq(canvasPosition["resourceType"], "service"),
          )
          .where(({ canvasPosition }) =>
            eq(canvasPosition["resourceId"], service.id),
          )
          .findOne(),
      ),
      variables: toArray(
        q
          .from({ variable: collections.variables })
          .where(({ variable }) => eq(variable.serviceId, service.id)),
      ),
    }));
}

type RawEnvironmentServiceViewRecord = GetResult<
  ExtractContext<ReturnType<typeof buildEnvironmentServicesViewQuery>>
>;

export type EnvironmentServiceRecord = Omit<
  ServiceWithContextRecord,
  "source" | "healthcheck" | "restartPolicy"
> & {
  source: ServiceSource;
  healthcheck: ServiceWithContextRecord["healthcheck"];
  restartPolicy: ServiceWithContextRecord["restartPolicy"];
  env: ServiceDeployEnv;
  mounts?: ServiceDeployMount[];
} & Pick<
    RawEnvironmentServiceViewRecord["service"],
    "$synced" | "$origin" | "$key" | "$collectionId"
  >;

export type EnvironmentServiceViewRecord = Omit<
  RawEnvironmentServiceViewRecord,
  "service" | "variables"
> & {
  service: EnvironmentServiceRecord;
  variables: VariableRecord[];
};

export function normalizeEnvironmentServicesViewRecord(
  record: RawEnvironmentServiceViewRecord,
): EnvironmentServiceViewRecord {
  const variables = record.variables.map((variable) =>
    parseLiveQueryRow(variableSelectSchema, variable),
  );

  return {
    ...record,
    variables,
    service: {
      ...record.service,
      source: decodeStrict(serviceSourceSchema, record.service.source),
      healthcheck: decodeStrict(serviceHealthcheckSchema, record.service.healthcheck),
      restartPolicy: decodeStrict(serviceRestartPolicySchema,
        record.service.restartPolicy,
      ),
      env: getDeployEnvFromServiceVariables({
        inlineVariables: variables,
        variableGroupAttachments: [],
      }),
    },
  };
}

export function projectServiceViewsWithBoundEnv(input: {
  services: EnvironmentServiceViewRecord[];
  environmentResources: VariableGroupResourceRecord[];
  attachments: EnvironmentServiceVariableGroupAttachment[];
}): EnvironmentServiceViewRecord[] {
  const resourceByVariableGroupId = new Map(
    input.environmentResources.map((resource) => [
      resource.variableGroup.id,
      resource,
    ]),
  );
  const attachmentsByServiceId = new Map<
    string,
    EnvironmentServiceVariableGroupAttachment[]
  >();

  for (const attachment of input.attachments) {
    const serviceAttachments =
      attachmentsByServiceId.get(attachment.serviceId) ?? [];
    serviceAttachments.push(attachment);
    attachmentsByServiceId.set(attachment.serviceId, serviceAttachments);
  }

  return input.services.map((serviceView) => {
    const attachments = attachmentsByServiceId.get(serviceView.service.id) ?? [];
    const variableGroupAttachments = attachments.map((attachment) => {
      const resource = resourceByVariableGroupId.get(attachment.variableGroupId);
      return {
        sortOrder: attachment.sortOrder,
        resourceId: resource?.resource.id,
        resourceName: resource?.resource.name,
        variableGroupId: resource?.variableGroup.id,
        variables: resource?.variables ?? [],
      };
    });
    if (variableGroupAttachments.length === 0) {
      return serviceView;
    }

    return {
      ...serviceView,
      service: {
        ...serviceView.service,
        env: getDeployEnvFromServiceVariables({
          inlineVariables: serviceView.variables,
          variableGroupAttachments,
        }),
      },
    };
  });
}

/**
 * Bind service-volume mounts onto each service view's deploy config. Mounts come
 * from active (non-tombstoned) volumes only, so a staged volume delete surfaces
 * as a mount removal on each consuming service (R19). Mirrors the env binding.
 */
export function projectServiceViewsWithBoundMounts(input: {
  services: EnvironmentServiceViewRecord[];
  volumeResources: VolumeResourceRecord[];
  attachments: EnvironmentServiceVolumeAttachment[];
}): EnvironmentServiceViewRecord[] {
  const volumeNameById = new Map<string, string>(
    input.volumeResources
      .filter((volume) => volume.resource.deletedAt == null)
      .map((volume) => [volume.resource.id, volume.resource.name]),
  );
  const mountsByServiceId = getServiceMountsByServiceId({
    attachments: input.attachments,
    volumeNameById,
  });

  return input.services.map((serviceView) => {
    const mounts: ServiceDeployMount[] =
      mountsByServiceId.get(serviceView.service.id) ?? [];
    if (mounts.length === 0 && (serviceView.service.mounts?.length ?? 0) === 0) {
      return serviceView;
    }

    return {
      ...serviceView,
      service: { ...serviceView.service, mounts },
    };
  });
}
