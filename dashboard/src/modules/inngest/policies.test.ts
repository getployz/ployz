import { describe, expect, it } from "vitest";
import { Inngest } from "inngest";
import {
  billingInngestFunctions,
  createScheduleNightlyBillingReconcile,
  createSyncOrganizationBillingStateFunction,
} from "#/modules/billing/inngest-sync/sync";
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
import { DRAIN_SLOT } from "#/modules/inngest/drain-slot";
import { createApplyServerPolicyChange } from "#/modules/machines/server-policy.inngest";
import { createCancelServerDrain, createCloseStaleServerDrains, createDrainServer } from "#/modules/machines/server-drain.inngest";
import {
  createScheduleClusterDomainSync,
  createSyncClusterDomain,
} from "#/modules/cluster-domain/sync.inngest";
import { createCancelServerUpgrade, createRollOutServerUpgrade, createScheduleServerUpgrades } from "#/modules/server-upgrade/server-upgrade.inngest";
import { createRetireServerAccess } from "#/modules/machines/server-access.inngest";
import { createPruneOrganizationChangeLog } from "#/modules/organization/change-log.inngest";

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
      { id: "process-machine-remove", retries: 5, concurrency: [{ key: "event.data.attemptId", limit: 1 }] },
      { id: "cancel-machine-remove", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "apply-server-policy-change", retries: 3, concurrency: [DRAIN_SLOT, { key: "event.data.machineId", limit: 1 }] },
      { id: "drain-server", retries: 3, concurrency: [DRAIN_SLOT] },
      { id: "cancel-server-drain", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "close-stale-server-drains", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "retire-server-access", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "prune-organization-change-log", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "sync-cluster-domain", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
      { id: "schedule-cluster-domain-sync", retries: 3, concurrency: [{ limit: 1 }] },
      { id: "roll-out-server-upgrade", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
      { id: "cancel-server-upgrade", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
      { id: "schedule-server-upgrades", retries: 3, concurrency: [{ limit: 1 }] },
    ]);
  });

  it("keys Drain and Server Policy changes with one expression, which Inngest needs to share a slot across functions", () => {
    const inngest = new Inngest({ id: "drain-slot-contract" });
    const [drainSlot] = [createDrainServer(inngest).opts.concurrency].flat();
    const [policySlot] = [createApplyServerPolicyChange(inngest).opts.concurrency].flat();
    expect(drainSlot).toMatchObject({ scope: "env", limit: 1 });
    expect(policySlot).toEqual(drainSlot);
  });

  it("registers billing sync only on hosted Cloud", () => {
    const inngest = new Inngest({ id: "registration-contract" });
    const ids = (mode: "hosted" | "self_hosted") =>
      billingInngestFunctions(inngest, mode).map(({ opts }) => opts.id);

    expect(ids("hosted")).toEqual(["sync-organization-billing-state", "schedule-nightly-billing-reconcile"]);
    expect(ids("self_hosted")).toEqual([]);
  });
});
