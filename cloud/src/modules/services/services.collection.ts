import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { parseServiceConfig } from "@ployz/sdk/config";
import { variableDocumentRecord } from "#/modules/environment-design/variable-document";
import { getEnvironmentDocumentsCollection } from "#/modules/environment-design/environment-document.collection";
import { serviceDocumentRecord } from "#/modules/environment-design/service-document";
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
  getRawServicesCollection,
  getRawEnvironmentResourcesCollection,
  getResourceLineagesCollection,
  getEnvironmentNodeConfigSnapshotsCollection,
  getVolumeRemoveAttemptsCollection,
} from "#/collections/collections";
import { getOrganizationDeploymentsCollection } from "#/modules/deployments/deployment-collection";
import {
  createEnvironmentResourcesCollection,
  createVolumeResourcesCollection,
} from "#/modules/environment-design/resource-collections";
import { updateServiceServerFn } from "#/modules/environment-design/service-functions";
import {
  type ServiceDeploymentFieldSelection,
  type ServiceDeployEnv,
  type ServiceDeployMount,
  type ServiceSource,
  type ServiceCanvasPositionRecord,
  type ServiceWithContextRecord,
} from "#/modules/environment-design/services";
import {
  createVariableWriter,
} from "#/modules/environment-design/variable-collections";
import {
  type VariableRecord,
} from "#/modules/environment-design/variables";

export type EnvironmentParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

function createServicesCollection(organizationSlug: string, scope: CollectionScope) {
  const identities = getRawServicesCollection(organizationSlug, scope);
  const documents = getEnvironmentDocumentsCollection(organizationSlug, scope);
  return createLiveQueryCollection({
    id: `collections:${organizationSlug}:services-with-context`,
    query: (q) => q.from({ identity: identities })
      .innerJoin({ document: documents }, ({ identity, document }) => eq(identity.environmentId, document.id))
      .fn.where(({ identity, document }) => document.intent.services.some((node) => node.id === identity.id))
      .fn.select(({ identity, document }) => {
        const node = document.intent.services.find((node) => node.id === identity.id);
        if (!node) throw new Error("Service is absent from the environment document.");
        return { ...serviceDocumentRecord(identity, node), projectSlug: document.projectSlug,
          environmentSlug: document.namespace };
      }),
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
  scope: CollectionScope,
  services: ReturnType<typeof createServicesCollection>,
): ServiceWriter {
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  type Edit = { serviceId: string; environmentId: string; revision: string;
    settings: ServiceDeploymentFieldSelection };
  const persist = createOptimisticAction<Edit>({
    onMutate: ({ environmentId, serviceId, settings }) => {
      environments.update(environmentId, (draft) => {
        if (settings.deletedAt) {
          draft.intent.services = draft.intent.services.filter((node) => node.id !== serviceId);
        } else {
          const node = draft.intent.services.find((node) => node.id === serviceId);
          if (!node) throw new Error("Service is not loaded.");
          const { deletedAt: _deletedAt, ...config } = settings;
          Object.assign(node.config, config);
        }
      });
    },
    mutationFn: async ({ serviceId, environmentId, revision, settings }) => {
      try {
        const result = await updateServiceServerFn({
          data: { organizationSlug, environmentId, serviceId, revision, ...settings },
        });
        await environments.writeCommitted(result.data);
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Something went wrong while saving this field.");
        throw error;
      }
    },
  });
  return {
    update(serviceId, updater) {
      const current = services.get(serviceId);
      if (!current) throw new Error("Service is not loaded.");
      const document = environments.get(current.environmentId);
      if (!document) throw new Error("Environment is not loaded.");
      const modified = structuredClone(current);
      updater(modified);
      const settings: ServiceDeploymentFieldSelection = {
        name: modified.name, source: modified.source,
        preDeployCommand: modified.preDeployCommand, startCommand: modified.startCommand,
        healthcheck: modified.healthcheck, restartPolicy: modified.restartPolicy,
        maxRetries: modified.maxRetries, cron: modified.cron, replicas: modified.replicas,
        cpuLimit: modified.cpuLimit, memLimit: modified.memLimit, privateDns: modified.privateDns,
        routes: modified.routes, managedHostname: modified.managedHostname, build: modified.build,
        deletedAt: modified.deletedAt ?? null,
      };
      return persist({ serviceId, environmentId: document.id, revision: document.revision, settings });
    },
  };
}

export function getCanvasPositionCollectionKey(
  item: Pick<ServiceCanvasPositionRecord, "resourceType" | "resourceId">,
) {
  return `${item.resourceType}:${item.resourceId}`;
}

export const getServicesCollection = cachedByCollectionScope(createServicesCollection);

const getServiceWriter = cachedByCollectionScope((organizationSlug, scope) =>
  createServiceWriter(
    organizationSlug,
    scope,
    getServicesCollection(organizationSlug, scope),
  ),
);

const getVariableWriter = cachedByCollectionScope(createVariableWriter);

function resourceSources(organizationSlug: string, scope: CollectionScope) {
  return {
    resources: getRawEnvironmentResourcesCollection(organizationSlug, scope),
    lineages: getResourceLineagesCollection(organizationSlug, scope),
    positions: getCanvasPositionsCollection(organizationSlug, scope),
    documents: getEnvironmentDocumentsCollection(organizationSlug, scope),
  };
}

export const getEnvironmentResourcesCollection = cachedByCollectionScope(
  (organizationSlug, scope) =>
    createEnvironmentResourcesCollection({
      organizationSlug,
      sources: resourceSources(organizationSlug, scope),
    }),
);

export const getVolumeResourcesCollection = cachedByCollectionScope(
  (organizationSlug, scope) =>
    createVolumeResourcesCollection({
      organizationSlug,
      sources: {
        ...resourceSources(organizationSlug, scope),
        snapshots: getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope),
        removals: getVolumeRemoveAttemptsCollection(organizationSlug, scope),
      },
    }),
);

export type ServicesCollection = ReturnType<typeof getServicesCollection>;
export function useServicesCollection(organizationSlug: string) {
  return getServicesCollection(organizationSlug, useCollectionScope());
}
export function useServiceWriter(organizationSlug: string) {
  return getServiceWriter(organizationSlug, useCollectionScope());
}
export function useEnvironmentResourcesCollection(organizationSlug: string) {
  return getEnvironmentResourcesCollection(organizationSlug, useCollectionScope());
}
export function useVolumeResourcesCollection(organizationSlug: string) {
  return getVolumeResourcesCollection(organizationSlug, useCollectionScope());
}
export function useCanvasPositionsCollection(organizationSlug: string) {
  return getCanvasPositionsCollection(organizationSlug, useCollectionScope());
}
export function useDeploymentsCollection(organizationSlug: string) {
  return getOrganizationDeploymentsCollection(organizationSlug, useCollectionScope());
}
export function useVariableWriter(organizationSlug: string) {
  return getVariableWriter(organizationSlug, useCollectionScope());
}

export function buildEnvironmentServicesViewQuery(
  q: InitialQueryBuilder,
  params: EnvironmentParams,
  collections: {
    services: ServicesCollection;
    canvasPositions: ReturnType<typeof getCanvasPositionsCollection>;
    documents: ReturnType<typeof getEnvironmentDocumentsCollection>;
  },
) {
  return q
    .from({ service: collections.services })
    .innerJoin({ document: collections.documents }, ({ service, document }) => eq(service.environmentId, document.id))
    .where(({ service }) => eq(service.projectSlug, params.projectSlug))
    .where(({ service }) => eq(service.environmentSlug, params.environmentSlug))
    .select(({ service, document }) => ({
      service, document,
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
  variableGroupAttachments?: ServiceDeploymentFieldSelection["variableGroupAttachments"];
} & Pick<
    RawEnvironmentServiceViewRecord["service"],
    "$synced" | "$origin" | "$key" | "$collectionId"
  >;

export type EnvironmentServiceViewRecord = Omit<
  RawEnvironmentServiceViewRecord,
  "service" | "document"
> & {
  service: EnvironmentServiceRecord;
  variables: VariableRecord[];
};

export function normalizeEnvironmentServicesViewRecord(
  record: RawEnvironmentServiceViewRecord,
): EnvironmentServiceViewRecord {
  const node = record.document.intent.services.find((node) => node.id === record.service.id);
  const snapshot = record.document.compiled.nodeSnapshots.find((node) => node.nodeType === "service" && node.nodeId === record.service.id);
  if (!node || !snapshot) throw new Error("Service is absent from the environment document.");
  const config = parseServiceConfig(snapshot.config);
  const { document: _document, ...view } = record;
  return {
    ...view,
    variables: node.variables.map((variable) => variableDocumentRecord(variable,
      { serviceId: node.id, variableGroupId: null }, record.document.intent, record.document.updatedAt)),
    service: { ...record.service, source: config.source, healthcheck: config.healthcheck,
      restartPolicy: config.restartPolicy, env: config.env, mounts: config.mounts,
      variableGroupAttachments: config.variableGroupAttachments },
  };
}
