import type { PloyzInngest } from "#/modules/inngest/client";
import {
  createScheduleNightlyBillingReconcile,
  createSyncOrganizationBillingStateFunction,
} from "#/modules/billing/inngest-sync/sync";
import {
  createProcessGithubCheckSuiteReceived,
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
  createMarkCancelledRowBackedWorkflow,
  createProcessEnvironmentDeployment,
} from "#/modules/deployments/environment-deployment.inngest";
import {
  createCancelMachineRemove,
  createProcessMachineRemove,
} from "#/modules/machines/machine-removal.inngest";
import {
  createCancelTeardown,
  createProcessTeardown,
} from "#/modules/runtime/teardown.inngest";
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
    createSweepGithubIngestionOutboxes(inngest),
    createSyncGithubRepositories(inngest),
    createMarkCancelledRowBackedWorkflow(inngest),
    createProcessEnvironmentDeployment(inngest),
    createScheduleGithubRepositorySync(inngest),
    createSyncOrganizationBillingStateFunction(inngest),
    createScheduleNightlyBillingReconcile(inngest),
    createProcessMachineRemove(inngest),
    createCancelMachineRemove(inngest),
    createProcessVolumeRemove(inngest),
    createCancelVolumeRemove(inngest),
    createProcessTeardown(inngest),
    createCancelTeardown(inngest),
  ];
}
