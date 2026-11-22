import { describe, expect, it } from "vitest";
import { Inngest } from "inngest";
import { DRAIN_SLOT } from "#/modules/machines/drain-slot";
import { createApplyServerPolicyChange } from "#/modules/machines/server-policy.inngest";
import { createDrainServer } from "#/modules/machines/server-drain.inngest";
import { createUptimeMaintenanceFunctions } from "#/modules/uptime/maintenance.inngest";
import { createWebAnalyticsFunctions } from "#/modules/web-analytics/web-analytics.inngest";
import { createScheduleAnonymousSweep } from "#/modules/sandbox/teardown.inngest";
import { createWorkerFunctions } from "#/worker/functions";

const PINNED_POLICIES = [
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
  { id: "sweep-expired-verifications", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "sync-cluster-domain", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
  { id: "schedule-cluster-domain-sync", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "ensure-cluster-sealing-key", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
  { id: "roll-out-server-upgrade", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
  { id: "cancel-server-upgrade", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
  { id: "schedule-server-upgrades", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "uptime-rollup", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "uptime-prune", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "uptime-backfill", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "analytics-retention", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "tear-down-anonymous-organization", retries: 3, concurrency: [{ key: "event.data.organizationId", limit: 1 }] },
  { id: "schedule-anonymous-sweep", retries: 3, concurrency: [{ limit: 1 }] },
  { id: "retire-old", retries: 3, concurrency: [{ key: "event.data.environmentId", limit: 1 }] },
  { id: "cancel-retire-old", retries: 3, concurrency: [{ key: "event.data.run_id", limit: 1 }] },
  { id: "close-stale-deployment-cleanups", retries: 3, concurrency: [{ limit: 1 }] },
] as const;

describe("Inngest function policies", () => {
  it("pins the SDK-default retry count and domain-owned concurrency", () => {
    const registered = new Map(
      createWorkerFunctions(new Inngest({ id: "policy-contract" }), "hosted").map(({ opts }) => [opts.id, opts]),
    );
    const policyOf = (id: string) => {
      const opts = registered.get(id);
      if (opts === undefined) throw new Error(`The worker does not register ${id}.`);
      return { id, retries: opts.retries, concurrency: opts.concurrency };
    };

    expect(PINNED_POLICIES.map(({ id }) => policyOf(id))).toEqual(PINNED_POLICIES);
  });

  it("registers every function once, billing sync only on hosted Cloud", () => {
    const ids = (mode: "hosted" | "self_hosted") =>
      createWorkerFunctions(new Inngest({ id: "registration-contract" }), mode).map(({ opts }) => opts.id);

    expect(new Set(ids("hosted")).size).toBe(ids("hosted").length);
    expect(ids("hosted").filter((id) => !ids("self_hosted").includes(id)))
      .toEqual(["sync-organization-billing-state", "schedule-nightly-billing-reconcile"]);
  });

  it("rolls uptime up at :05, once the previous hour has closed", () => {
    const [rollup] = createUptimeMaintenanceFunctions(new Inngest({ id: "uptime-contract" }));
    expect(rollup?.opts).toMatchObject({ id: "uptime-rollup", triggers: [{ cron: "5 * * * *" }] });
  });

  it("sweeps lapsed anonymous Organizations every 15 minutes", () => {
    const sweep = createScheduleAnonymousSweep(new Inngest({ id: "anonymous-contract" }));
    expect(sweep.opts).toMatchObject({ id: "schedule-anonymous-sweep", triggers: [{ cron: "TZ=UTC */15 * * * *" }] });
  });

  it("runs analytics retention daily on a UTC cron", () => {
    const [retention] = createWebAnalyticsFunctions(new Inngest({ id: "analytics-contract" }));
    expect(retention?.opts).toMatchObject({ id: "analytics-retention", triggers: [{ cron: "TZ=UTC 20 3 * * *" }] });
  });

  it("keys Drain and Server Policy changes with one expression, which Inngest needs to share a slot across functions", () => {
    const inngest = new Inngest({ id: "drain-slot-contract" });
    const [drainSlot] = [createDrainServer(inngest).opts.concurrency].flat();
    const [policySlot] = [createApplyServerPolicyChange(inngest).opts.concurrency].flat();
    expect(drainSlot).toMatchObject({ scope: "env", limit: 1 });
    expect(policySlot).toEqual(drainSlot);
  });
});
