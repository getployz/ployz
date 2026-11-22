import { describe, expect, it } from "vitest";
import { Inngest } from "inngest";
import { createApplyServerPolicyChange } from "#/modules/machines/server-policy.inngest";
import { createDrainServer } from "#/modules/machines/server-drain.inngest";
import { createWorkerFunctions } from "#/worker/functions";

describe("Inngest function policies", () => {
  it("registers every function once, billing sync only on hosted Cloud", () => {
    const ids = (mode: "hosted" | "self_hosted") =>
      createWorkerFunctions(new Inngest({ id: "registration-contract" }), mode).map(({ opts }) => opts.id);

    expect(new Set(ids("hosted")).size).toBe(ids("hosted").length);
    expect(ids("hosted").filter((id) => !ids("self_hosted").includes(id)))
      .toEqual(["sync-organization-billing-state", "schedule-nightly-billing-reconcile"]);
  });

  it("keys Drain and Server Policy changes with one expression, which Inngest needs to share a slot across functions", () => {
    const inngest = new Inngest({ id: "drain-slot-contract" });
    const [drainSlot] = [createDrainServer(inngest).opts.concurrency].flat();
    const [policySlot] = [createApplyServerPolicyChange(inngest).opts.concurrency].flat();
    expect(drainSlot).toMatchObject({ scope: "env", limit: 1 });
    expect(policySlot).toEqual(drainSlot);
  });
});
