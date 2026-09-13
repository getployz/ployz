import { Data } from "effect";
import type {
  EnvironmentDeploymentPreview,
  EnvironmentDeploymentServiceActionPolicy,
  EnvironmentDeploymentStatus,
} from "#/modules/deployments/tables";
import type { EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";
import type { DeploymentTriggerOrigin } from "#/modules/deployments/deployment";
import type {
  EnvironmentDeploySnapshot,
  EnvironmentDeployVolume,
} from "#/modules/deployments/runtime-contract";

export class DeploymentQueueOccupied extends Data.TaggedError(
  "DeploymentQueueOccupied",
)<{ readonly cause: unknown }> {}

export type DeploymentContext = {
  deployment: {
    id: string;
    status: EnvironmentDeploymentStatus;
    environmentId: string;
    inngestRunId?: string | null;
    coreDeployId?: string | null;
    deployPreview?: EnvironmentDeploymentPreview | null;
    variableProducers?: EnvironmentSnapshotVariableProducer[] | null;
    triggerOrigin?: DeploymentTriggerOrigin;
    serviceActionPolicy?: EnvironmentDeploymentServiceActionPolicy | null;
  };
  environment: {
    id: string;
    namespace: string;
  };
  project: {
    id: string;
    organizationId: string;
  };
  organization: {
    id: string;
    slug: string;
  };
  snapshots: EnvironmentDeploySnapshot[];
  appliedServiceIds?: string[];
  volumes: EnvironmentDeployVolume[];
};
