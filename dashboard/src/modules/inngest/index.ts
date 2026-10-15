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
import { createServerDrainFunctions } from "#/modules/machines/server-drain.inngest";
import { createServerWebTrafficFunctions } from "#/modules/machines/server-web-traffic.inngest";
import { createNamespaceCleanupFunctions } from "#/modules/machines/namespace-cleanup.inngest";
import {
  createScheduleClusterDomainSync,
  createSyncClusterDomain,
} from "#/modules/cluster-domain/sync.inngest";
import { createServerUpgradeFunctions } from "#/modules/server-upgrade/server-upgrade.inngest";
import { createRetireServerAccess } from "#/modules/machines/server-access.inngest";
import { createPruneOrganizationChangeLog } from "#/modules/organization/change-log.inngest";
import {
  createCancelStoreDeployment, createDeployToFirstServer, createRedispatchStoreDeployments, createRunStoreDeployment,
} from "#/modules/config-store/store-deployment.inngest";
import {
  createStoreGithubCheckSuite, createStoreGithubPush, createStorePrCheck, createStorePullRequest, createStoreSweep,
  createStoreSweepRequested,
} from "#/modules/config-store/store-github.inngest";
import { createRefreshVolumeMirrors } from "#/modules/volume-placement/volume-placement.inngest";
import { createSendUptimeAlerts } from "#/modules/uptime/alert-mail.inngest";
import { createUptimeMaintenanceFunctions } from "#/modules/uptime/maintenance.inngest";
import { createVolumeRunFunctions } from "#/modules/volume-run/volume-run.inngest";
import { createSweepOperationOutbox } from "#/modules/operations/outbox.server";
import { createServiceActFunctions } from "#/modules/service-act/service-act.inngest";
import { createSendMail } from "#/modules/mail/mail.inngest";
import { createCrashMailFunctions } from "#/modules/observe-ingest/crash-mail.inngest";
import { createWebAnalyticsFunctions } from "#/modules/web-analytics/postgres/web-analytics.inngest";
import { createDeliverPushTargets } from "#/modules/observe-ingest/push-target.inngest";

export function createInngestFunctions(inngest: PloyzInngest) {
  return [
    createProcessGithubInstallationReceived(inngest),
    createProcessGithubInstallationRepositoriesReceived(inngest),
    createSyncGithubRepositories(inngest),
    createScheduleGithubRepositorySync(inngest),
    createProcessMachineRemove(inngest),
    createCancelMachineRemove(inngest),
    createApplyServerPolicyChange(inngest),
    ...createServerDrainFunctions(inngest),
    ...createServerWebTrafficFunctions(inngest),
    ...createNamespaceCleanupFunctions(inngest),
    createRetireServerAccess(inngest),
    createPruneOrganizationChangeLog(inngest),
    createSyncClusterDomain(inngest),
    createScheduleClusterDomainSync(inngest),
    ...createServerUpgradeFunctions(inngest),
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
    ...createVolumeRunFunctions(inngest),
    ...createServiceActFunctions(inngest),
    createSweepOperationOutbox(inngest),
    createRefreshVolumeMirrors(inngest),
    ...createUptimeMaintenanceFunctions(inngest),
    createSendUptimeAlerts(inngest),
    createSendMail(inngest),
    ...createCrashMailFunctions(inngest),
    createDeliverPushTargets(inngest),
    ...createWebAnalyticsFunctions(inngest),
  ];
}
