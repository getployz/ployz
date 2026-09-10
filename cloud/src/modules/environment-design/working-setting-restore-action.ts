import { createOptimisticAction, type Collection } from "@tanstack/react-db";
import { restoreServiceSetting } from "@ployz/sdk/config";
import { projectServiceDeploymentConfig, type ServiceDeploymentConfig } from "./services";
import type { EnvironmentDocument } from "./working-state-repository.server";
import type { RestoreWorkingDocumentInput } from "./working-document-restore";

export function createWorkingSettingRestoreAction({ environments, environmentId, organizationSlug, restore }: {
  environments: Pick<Collection<EnvironmentDocument>, "get" | "update"> & {
    writeCommitted: (row: EnvironmentDocument) => Promise<void>;
  };
  environmentId: string;
  organizationSlug: string;
  restore: (input: { data: RestoreWorkingDocumentInput }) => Promise<{ data: EnvironmentDocument }>;
}) {
  return createOptimisticAction<{
    serviceId: string;
    revision: string;
    path: string;
    baseline: ServiceDeploymentConfig;
    snapshotSource: RestoreWorkingDocumentInput["snapshotSource"];
  }>({
    onMutate: ({ serviceId, path, baseline }) => {
      const current = environments.get(environmentId)?.intent.services.find((node) => node.id === serviceId);
      if (!current) throw new Error("Service is not loaded.");
      const { env: _env, mounts: _mounts, variableGroupAttachments, ...config } = restoreServiceSetting(
        { ...projectServiceDeploymentConfig(current.config), variableGroupAttachments: current.variableGroupAttachments }, baseline, path,
      );
      environments.update(environmentId, (draft) => {
        const service = draft.intent.services.find((node) => node.id === serviceId);
        if (!service) throw new Error("Service is not loaded.");
        service.config = config;
        service.variableGroupAttachments = variableGroupAttachments;
      });
    },
    mutationFn: async ({ serviceId, revision, path, snapshotSource }) => {
      const result = await restore({ data: {
        organizationSlug: organizationSlug, environmentId, revision,
        snapshotSource,
        command: { kind: "node", nodeType: "service", nodeId: serviceId, path },
      } });
      await environments.writeCommitted(result.data);
    },
  });
}
