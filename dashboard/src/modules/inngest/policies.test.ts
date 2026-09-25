import { describe, expect, it } from "vitest";
import { Inngest } from "inngest";
import {
  billingInngestFunctions,
  createScheduleNightlyBillingReconcile,
  createSyncOrganizationBillingStateFunction,
} from "#/modules/billing/inngest-sync/sync";
import {
  createMarkCancelledRowBackedWorkflow,
  createProcessEnvironmentDeployment,
} from "#/modules/deployments/environment-deployment.inngest";
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
import { createPruneOrganizationChangeLog } from "#/modules/organization/change-log.inngest";
import {
  createCancelVolumeRemove,
  createProcessVolumeRemove,
} from "#/modules/runtime/volume-removal.inngest";

describe("Inngest function policies", () => {
  it("pins the SDK-default retry count and domain-owned concurrency", () => {
    const inngest = new Inngest({ id: "policy-contract" });
    const functions = [
      createSyncOrganizationBillingStateFunction(inngest),
      createScheduleNightlyBillingReconcile(inngest),
      createSyncGithubRepositories(inngest),
      createScheduleGithubRepositorySync(inngest),
      createProcessGithubInstallationReceived(inngest),
      createProcessGithubInstallationRepositoriesReceived(inngest),
      createProcessGithubPushReceived(inngest),
      createProcessGithubCheckSuiteReceived(inngest),
      createSweepGithubIngestionOutboxes(inngest),
      createProcessEnvironmentDeployment(inngest),
      createMarkCancelledRowBackedWorkflow(inngest),
      createProcessMachineRemove(inngest),
      createCancelMachineRemove(inngest),
      createApplyServerPolicyChange(inngest),
      createProcessTeardown(inngest),
      createCancelTeardown(inngest),
      createProcessVolumeRemove(inngest),
      createCancelVolumeRemove(inngest),
      createPruneOrganizationChangeLog(inngest),
    createSyncClusterDomain(inngest),
    createScheduleClusterDomainSync(inngest),
    ];

    expect(
      functions.map(({ opts }) => ({
        id: opts.id,
        retries: opts.retries,
        concurrency: opts.concurrency,
      })),
    ).toEqual([
      { id: "sync-organization-billing-state", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
      { id: "schedule-nightly-billing-reconcile", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "sync-github-repositories", retries: 3, concurrency: [{ key: "event.data.installationId", limit: 1 }] },
      { id: "schedule-github-repository-sync", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "process-github-installation-received", retries: 3, concurrency: [{ key: "event.data.installation.id", limit: 1 }] },
      { id: "process-github-installation-repositories-received", retries: 3, concurrency: [{ key: "event.data.installation.id", limit: 1 }] },
      { id: "process-github-push-received", retries: 5, concurrency: [{ key: "event.data.branchKey", limit: 1 }] },
      { id: "process-github-check-suite-received", retries: 5, concurrency: [{ key: "event.data.checkSuiteKey", limit: 1 }] },
      { id: "sweep-github-ingestion-outboxes", retries: 5, concurrency: [{ limit: 1 }] },
      { id: "process-environment-deployment", retries: 0, concurrency: [{ key: "event.data.environmentDeploymentId", limit: 32 }] },
      { id: "mark-cancelled-row-backed-workflow", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "process-machine-remove", retries: 5, concurrency: [{ key: "event.data.attemptId", limit: 1 }] },
      { id: "cancel-machine-remove", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "apply-server-policy-change", retries: 3, concurrency: [{ key: "event.data.machineId", limit: 1 }] },
      { id: "process-teardown", retries: 0, concurrency: [{ key: "event.data.attemptId", limit: 1 }] },
      { id: "cancel-teardown", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "process-volume-remove", retries: 0, concurrency: [{ key: "event.data.attemptId", limit: 1 }] },
      { id: "cancel-volume-remove", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "prune-organization-change-log", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "sync-cluster-domain", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
      { id: "schedule-cluster-domain-sync", retries: 3, concurrency: [{ limit: 1 }] },
    ]);
  });

  it("registers billing sync only on hosted Cloud", () => {
    const inngest = new Inngest({ id: "registration-contract" });
    const ids = (mode: "hosted" | "self_hosted") =>
      billingInngestFunctions(inngest, mode).map(({ opts }) => opts.id);

    expect(ids("hosted")).toEqual(["sync-organization-billing-state", "schedule-nightly-billing-reconcile"]);
    expect(ids("self_hosted")).toEqual([]);
  });
});
