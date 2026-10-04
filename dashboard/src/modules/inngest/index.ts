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
import { createCancelServerDrain, createCloseStaleServerDrains, createDrainServer } from "#/modules/machines/server-drain.inngest";
import {
  createScheduleClusterDomainSync,
  createSyncClusterDomain,
} from "#/modules/cluster-domain/sync.inngest";
import { createCancelServerUpgrade, createRollOutServerUpgrade, createScheduleServerUpgrades } from "#/modules/server-upgrade/server-upgrade.inngest";
import { createRetireServerAccess } from "#/modules/machines/server-access.inngest";
import { createPruneOrganizationChangeLog } from "#/modules/organization/change-log.inngest";
import {
  createCancelStoreDeployment, createDeployToFirstServer, createRedispatchStoreDeployments, createRunStoreDeployment,
} from "#/modules/config-store/store-deployment.inngest";
import {
  createStoreGithubCheckSuite, createStoreGithubPush, createStorePrCheck, createStorePullRequest, createStoreSweep,
  createStoreSweepRequested,
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
    createDrainServer(inngest),
    createCancelServerDrain(inngest),
    createCloseStaleServerDrains(inngest),
    createRetireServerAccess(inngest),
    createPruneOrganizationChangeLog(inngest),
    createSyncClusterDomain(inngest),
    createScheduleClusterDomainSync(inngest),
    createRollOutServerUpgrade(inngest),
    createCancelServerUpgrade(inngest),
    createScheduleServerUpgrades(inngest),
    createRunStoreDeployment(inngest),
    createCancelStoreDeployment(inngest),
    createRedispatchStoreDeployments(inngest),
    createDeployToFirstServer(inngest),
    createStoreGithubPush(inngest),
    createStoreGithubCheckSuite(inngest),
    createStorePullRequest(inngest),
    createStorePrCheck(inngest),
    createStoreSweep(inngest),
    createStoreSweepRequested(inngest),
  ];
}
