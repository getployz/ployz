import { billingInngestFunctions } from "#/modules/billing/inngest-sync/sync";
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
import { createServerEnrollmentFunctions } from "#/modules/machines/waiting/server-enrollment.inngest";
import { createIngressDoorsFunctions } from "#/modules/ingress/doors.inngest";
import { createServerWebTrafficFunctions } from "#/modules/machines/server-web-traffic.inngest";
import { createNamespaceCleanupFunctions } from "#/modules/machines/namespace-cleanup.inngest";
import { createEnsureClusterSealingKey } from "#/modules/sealing/sealing-backfill.inngest";
import { createResealVariables } from "#/modules/sealing/reseal.inngest";
import {
  createScheduleClusterDomainSync,
  createSyncClusterDomain,
} from "#/modules/cluster-domain/sync.inngest";
import { createServerUpgradeFunctions } from "#/modules/server-upgrade/server-upgrade.inngest";
import { createClusterNameFunctions } from "#/modules/cluster-name/cluster-name.inngest";
import { createRetireServerAccess } from "#/modules/machines/server-access.inngest";
import { createPruneOrganizationChangeLog } from "#/collections/change-log/change-log.inngest";
import { createSweepExpiredVerifications } from "#/modules/identity/verification-sweep.inngest";
import {
  createCancelStoreDeployment, createDeployToFirstServer, createRedispatchStoreDeployments, createRunStoreDeployment,
} from "#/modules/config-store/store-deployment.inngest";
import { createStopStoreDeployment } from "#/modules/config-store/store-deployment-stop.inngest";
import {
  createStoreGithubCheckSuite, createStoreGithubPush, createStorePrCheck, createStorePullRequest, createStoreSweep,
  createStoreSweepRequested,
} from "#/modules/config-store/store-github.inngest";
import { createRefreshVolumeMirrors } from "#/modules/volume-copies/mirror-sync/mirror-sync.inngest";
import { createSendUptimeAlerts } from "#/modules/uptime/alert-mail.inngest";
import { createUptimeMaintenanceFunctions } from "#/modules/uptime/maintenance.inngest";
import { createVolumeRunFunctions } from "#/modules/volume-run/volume-run.inngest";
import { createSweepOperationOutbox } from "#/modules/inngest/outbox.server";
import { createSweepProposals } from "#/modules/proposals/proposals.inngest";
import { createServiceActFunctions } from "#/modules/service-act/service-act.inngest";
import { createSendMail } from "#/modules/mail/mail.inngest";
import { createCrashMailFunctions } from "#/modules/observe-ingest/crash-mail.inngest";
import { createWebAnalyticsFunctions } from "#/modules/web-analytics/web-analytics.inngest";
import { createDeliverPushTargets } from "#/modules/observe-ingest/push-target.inngest";
import { createScheduleAnonymousSweep, createTearDownAnonymousOrganization } from "#/modules/sandbox/teardown.inngest";
import type { PolarConfiguration } from "#/server/config.server";

/** Every function the worker registers. A new function goes here, and nowhere else. */
export function createWorkerFunctions(inngest: PloyzInngest, billingMode: PolarConfiguration["mode"]) {
  return [
    createProcessGithubInstallationReceived(inngest),
    createProcessGithubInstallationRepositoriesReceived(inngest),
    createSyncGithubRepositories(inngest),
    createScheduleGithubRepositorySync(inngest),
    createProcessMachineRemove(inngest),
    createCancelMachineRemove(inngest),
    createApplyServerPolicyChange(inngest),
    ...createServerDrainFunctions(inngest),
    ...createServerEnrollmentFunctions(inngest),
    ...createServerWebTrafficFunctions(inngest),
    ...createIngressDoorsFunctions(inngest),
    ...createNamespaceCleanupFunctions(inngest),
    createRetireServerAccess(inngest),
    createPruneOrganizationChangeLog(inngest),
    createSweepExpiredVerifications(inngest),
    createSyncClusterDomain(inngest),
    createScheduleClusterDomainSync(inngest),
    ...createClusterNameFunctions(inngest),
    createEnsureClusterSealingKey(inngest),
    createResealVariables(inngest),
    ...createServerUpgradeFunctions(inngest),
    createRunStoreDeployment(inngest),
    createCancelStoreDeployment(inngest),
    createStopStoreDeployment(inngest),
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
    createSweepProposals(inngest),
    createSweepOperationOutbox(inngest),
    createRefreshVolumeMirrors(inngest),
    ...createUptimeMaintenanceFunctions(inngest),
    createSendUptimeAlerts(inngest),
    createSendMail(inngest),
    ...createCrashMailFunctions(inngest),
    createDeliverPushTargets(inngest),
    ...createWebAnalyticsFunctions(inngest),
    createTearDownAnonymousOrganization(inngest),
    createScheduleAnonymousSweep(inngest),
    ...billingInngestFunctions(inngest, billingMode),
  ];
}
