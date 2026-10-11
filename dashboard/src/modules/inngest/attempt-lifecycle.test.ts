import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { createCancelServerDrain, createServerDrainFunctions } from "#/modules/machines/server-drain.inngest";
import { createCancelServerUpgrade, createServerUpgradeFunctions } from "#/modules/server-upgrade/server-upgrade.inngest";
import { createCancelVolumeRun, createVolumeRunFunctions } from "#/modules/volume-run/volume-run.inngest";

describe("attemptLifecycle", () => {
  it.each([
    ["drain-server", createServerDrainFunctions, "server/drain.requested", "cancel-server-drain", "close-stale-server-drains"],
    ["roll-out-server-upgrade", createServerUpgradeFunctions, "server/upgrade.requested", "cancel-server-upgrade", "schedule-server-upgrades"],
    ["run-volume", createVolumeRunFunctions, "volume/run.requested", "cancel-volume-run", "close-stale-volume-runs"],
  ])("registers %s with its failure handler, its cancel handler and its hourly sweep", (id, create, event, cancelId, sweepId) => {
    const [run, cancel, sweep] = create(new Inngest({ id: "test" }));

    expect(run.opts).toMatchObject({ id, triggers: [{ event: { name: event } }], onFailure: expect.any(Function) });
    expect(cancel.opts).toMatchObject({ id: cancelId, triggers: [{ event: { name: "inngest/function.cancelled" } }] });
    expect(sweep.opts).toMatchObject({ id: sweepId, triggers: [{ cron: "TZ=UTC 0 * * * *" }] });
  });

  it.each([
    ["ployz-cloud-drain-server", createCancelServerDrain, "ployz-cloud-roll-out-server-upgrade"],
    ["ployz-cloud-roll-out-server-upgrade", createCancelServerUpgrade, "ployz-cloud-run-volume"],
    ["ployz-cloud-run-volume", createCancelVolumeRun, "ployz-cloud-drain-server"],
  ])("closes the run when Inngest cancels %s, and skips another function's cancellation", async (functionId, createCancel, otherFunctionId) => {
    const cancel = (cancelled: string) => new InngestTestEngine({
      function: createCancel(new Inngest({ id: "ployz-cloud" })),
      events: [{ name: "inngest/function.cancelled", data: { function_id: cancelled, run_id: "run-1" } }],
      steps: [{ id: "close-run", handler: () => 1 }],
    }).execute();

    expect((await cancel(functionId)).result).toEqual({ closed: 1 });
    expect((await cancel(otherFunctionId)).result).toEqual({ skipped: true });
  });

  it("names the cancelled function under the client's app id", async () => {
    const cancel = await new InngestTestEngine({
      function: createCancelServerDrain(new Inngest({ id: "self-hosted" })),
      events: [{ name: "inngest/function.cancelled", data: { function_id: "self-hosted-drain-server", run_id: "run-1" } }],
      steps: [{ id: "close-run", handler: () => 1 }],
    }).execute();

    expect(cancel.result).toEqual({ closed: 1 });
  });

  it("passes a singleton through, so a second run of one Volume is skipped while the first runs", () => {
    const [run] = createVolumeRunFunctions(new Inngest({ id: "test" }));

    expect(run.opts.singleton).toEqual({ key: "event.data.volumeId", mode: "skip" });
  });

  it("leaves singleton unset on a run that does not ask for it", () => {
    const [run] = createServerDrainFunctions(new Inngest({ id: "test" }));

    expect(run.opts.singleton).toBeUndefined();
  });
});
