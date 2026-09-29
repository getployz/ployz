import type { PloyzInngest } from "#/modules/inngest/client";
import { createScheduleGithubRepositorySync } from "#/modules/github/inngest-sync/scheduled";
import { createSyncGithubRepositories } from "#/modules/github/inngest-sync/sync";
import {
  createProcessGithubInstallationReceived,
  createProcessGithubInstallationRepositoriesReceived,
} from "#/modules/github/inngest-sync/webhook";
import {
  createCancelMachineRemove,
  createProcessMachineRemove,
} from "#/modules/machines/machine-removal.inngest";
import { createApplyServerPolicyChange } from "#/modules/machines/server-policy.inngest";
import {
  createScheduleClusterDomainSync,
  createSyncClusterDomain,
} from "#/modules/cluster-domain/sync.inngest";
import { createRetireServerAccess } from "#/modules/machines/server-access.inngest";
import { createPruneOrganizationChangeLog } from "#/modules/organization/change-log.inngest";
import { createCancelStoreDeployment, createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import {
  createStoreGithubCheckSuite, createStoreGithubPush, createStorePrCheck, createStorePullRequest, createStoreSweep,
} from "#/modules/config-store/store-github.inngest";

export function createInngestFunctions(inngest: PloyzInngest) {
  return [
    createProcessGithubInstallationReceived(inngest),
    createProcessGithubInstallationRepositoriesReceived(inngest),
    createSyncGithubRepositories(inngest),
    createScheduleGithubRepositorySync(inngest),
    createProcessMachineRemove(inngest),
    createCancelMachineRemove(inngest),
    createApplyServerPolicyChange(inngest),
    createRetireServerAccess(inngest),
    createPruneOrganizationChangeLog(inngest),
    createSyncClusterDomain(inngest),
    createScheduleClusterDomainSync(inngest),
    createRunStoreDeployment(inngest),
    createCancelStoreDeployment(inngest),
    createStoreGithubPush(inngest),
    createStoreGithubCheckSuite(inngest),
    createStorePullRequest(inngest),
    createStorePrCheck(inngest),
    createStoreSweep(inngest),
  ];
}
