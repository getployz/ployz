import { describe, expect, it } from "vitest";
import { Inngest } from "inngest";
import { createWorkerFunctions } from "#/worker/functions";

describe("worker functions", () => {
  it("registers every function once, billing sync only on hosted Cloud", () => {
    const ids = (mode: "hosted" | "self_hosted") =>
      createWorkerFunctions(new Inngest({ id: "registration-contract" }), mode).map(({ opts }) => opts.id);

    expect(new Set(ids("hosted")).size).toBe(ids("hosted").length);
    expect(ids("hosted").filter((id) => !ids("self_hosted").includes(id)))
      .toEqual(["sync-organization-billing-state", "schedule-nightly-billing-reconcile"]);
  });
});
