import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { createServerDrainFunctions } from "#/modules/machines/server-drain.inngest";
import { createServerUpgradeFunctions } from "#/modules/server-upgrade/server-upgrade.inngest";

describe("attemptLifecycle", () => {
  it.each([
    ["drain-server", createServerDrainFunctions, "server/drain.requested", "cancel-server-drain", "close-stale-server-drains"],
    ["roll-out-server-upgrade", createServerUpgradeFunctions, "server/upgrade.requested", "cancel-server-upgrade", "schedule-server-upgrades"],
  ])("registers %s with its failure handler, its cancel handler and its hourly sweep", (id, create, event, cancelId, sweepId) => {
    const [run, cancel, sweep] = create(new Inngest({ id: "test" }));

    expect(run.opts).toMatchObject({ id, triggers: [{ event: { name: event } }], onFailure: expect.any(Function) });
    expect(cancel.opts).toMatchObject({ id: cancelId, triggers: [{ event: { name: "inngest/function.cancelled" } }] });
    expect(sweep.opts).toMatchObject({ id: sweepId, triggers: [{ cron: "TZ=UTC 0 * * * *" }] });
  });
});
