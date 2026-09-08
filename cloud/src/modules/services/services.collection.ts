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
} from "#/electric/collections";
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

function createServicesCollection(organizationSlug: string) {
  const identities = getRawServicesCollection(organizationSlug);
  const documents = getEnvironmentDocumentsCollection(organizationSlug);
  return createLiveQueryCollection({
    id: `electric:${organizationSlug}:services-with-context`,
    startSync: true,
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
  services: ReturnType<typeof createServicesCollection>,
): ServiceWriter {
  const environments = getEnvironmentsCollection(organizationSlug);
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
        const receipt = await updateServiceServerFn({
          data: { organizationSlug, environmentId, serviceId, revision, ...settings },
        });
        await environments.utils.awaitTxId(receipt.txid);
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

const getVariableWriter = cachedByOrganization(createVariableWriter);

function resourceSources(organizationSlug: string) {
  return {
    resources: getRawEnvironmentResourcesCollection(organizationSlug),
    lineages: getResourceLineagesCollection(organizationSlug),
    positions: getCanvasPositionsCollection(organizationSlug),
    documents: getEnvironmentDocumentsCollection(organizationSlug),
  };
}

const getEnvironmentResourcesCollection = cachedByOrganization(
  (organizationSlug) =>
    createEnvironmentResourcesCollection({
      organizationSlug,
      sources: resourceSources(organizationSlug),
    }),
);

const getVolumeResourcesCollection = cachedByOrganization(
  (organizationSlug) =>
    createVolumeResourcesCollection({
      organizationSlug,
      sources: {
        ...resourceSources(organizationSlug),
        snapshots: getEnvironmentNodeConfigSnapshotsCollection(organizationSlug),
        removals: getVolumeRemoveAttemptsCollection(organizationSlug),
      },
    }),
);

export type ServicesCollection = ReturnType<typeof getServicesCollection>;
export const useServicesCollection = getServicesCollection;
export const useServiceWriter = getServiceWriter;
export const useEnvironmentResourcesCollection =
  getEnvironmentResourcesCollection;
export const useVolumeResourcesCollection = getVolumeResourcesCollection;
export const useCanvasPositionsCollection = getCanvasPositionsCollection;
export const useDeploymentsCollection = getOrganizationDeploymentsCollection;
export const useVariableWriter = getVariableWriter;

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
