import type { PloyzInngest } from "#/modules/inngest/client";
import {
  createProcessGithubCheckSuiteReceived,
  createProcessGithubPullRequestReceived,
  createProcessGithubPushReceived,
} from "#/modules/github/inngest-ingestion/process";
import { createSweepGithubIngestionOutboxes } from "#/modules/github/inngest-ingestion/sweep";
import { createScheduleGithubRepositorySync } from "#/modules/github/inngest-sync/scheduled";
import { createSyncGithubRepositories } from "#/modules/github/inngest-sync/sync";
import {
  createProcessGithubInstallationReceived,
  createProcessGithubInstallationRepositoriesReceived,
} from "#/modules/github/inngest-sync/webhook";
import {
  createDispatchStrandedPendingDeployments,
  createMarkCancelledRowBackedWorkflow,
  createProcessEnvironmentDeployment,
} from "#/modules/deployments/environment-deployment.inngest";
import {
  createCancelMachineRemove,
  createProcessMachineRemove,
} from "#/modules/machines/machine-removal.inngest";
import { createApplyServerPolicyChange } from "#/modules/machines/server-policy.inngest";
import {
  createCancelTeardown,
  createProcessTeardown,
} from "#/modules/runtime/teardown.inngest";
import {
  createScheduleClusterDomainSync,
  createSyncClusterDomain,
} from "#/modules/cluster-domain/sync.inngest";
import { createSweepIdleBranches } from "#/modules/branches/branch-sweep.inngest";
import { createPruneOrganizationChangeLog } from "#/modules/organization/change-log.inngest";
import {
  createCancelVolumeRemove,
  createProcessVolumeRemove,
} from "#/modules/runtime/volume-removal.inngest";

export function createInngestFunctions(inngest: PloyzInngest) {
  return [
    createProcessGithubInstallationReceived(inngest),
    createProcessGithubInstallationRepositoriesReceived(inngest),
    createProcessGithubPushReceived(inngest),
    createProcessGithubCheckSuiteReceived(inngest),
    createProcessGithubPullRequestReceived(inngest),
    createSweepGithubIngestionOutboxes(inngest),
    createSyncGithubRepositories(inngest),
    createMarkCancelledRowBackedWorkflow(inngest),
    createProcessEnvironmentDeployment(inngest),
    createDispatchStrandedPendingDeployments(inngest),
    createScheduleGithubRepositorySync(inngest),
    createProcessMachineRemove(inngest),
    createCancelMachineRemove(inngest),
    createApplyServerPolicyChange(inngest),
    createProcessVolumeRemove(inngest),
    createCancelVolumeRemove(inngest),
    createProcessTeardown(inngest),
    createCancelTeardown(inngest),
    createPruneOrganizationChangeLog(inngest),
    createSweepIdleBranches(inngest),
    createSyncClusterDomain(inngest),
    createScheduleClusterDomainSync(inngest),
  ];
}
