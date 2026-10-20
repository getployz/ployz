import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createCancelRetireOld, createRetireOld } from "#/modules/config-store/retire-old.inngest";
import * as cleanupRows from "#/modules/config-store/retire-old.server";
import type { CleanupLeft } from "#/modules/config-store/tables";
import { configDeploymentRetireRequestedEvent } from "#/modules/inngest/events";
import { invokeOnFailure } from "#/test/inngest-failure";
import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";

useServiceFreeEffectRunner();

const machine3: CleanupLeft = { machine: "machine-3", service: "web", reason: "machine-3 did not answer" };
let answers: CleanupLeft[][] = [];
let writes: object[] = [];

beforeEach(() => {
  writes = [];
  vi.spyOn(cleanupRows, "retireOldCopies").mockImplementation(() => Effect.succeed({ left: answers.shift() ?? [] }));
  vi.spyOn(cleanupRows, "recordCleanupWaiting").mockImplementation((runId, _data, left = []) =>
    Effect.sync(() => { writes.push({ waiting: runId, left }); }));
  vi.spyOn(cleanupRows, "endCleanup").mockImplementation((runId, end) => Effect.sync(() => writes.push({ end: runId, ...end })));
});

const requested = {
  name: configDeploymentRetireRequestedEvent,
  data: { organizationId: "org", environmentId: "env", deploymentId: "dep" },
};

/** Runs a cleanup whose attempts leave `left` in turn. */
async function cleanup(left: CleanupLeft[][]) {
  answers = left;
  const { result, ctx } = await new InngestTestEngine({
    function: createRetireOld(new Inngest({ id: "retire-old-test" })),
    events: [requested],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId: "run-1" }),
    // Each pause ends at once.
    steps: left.map((_, index) => ({ id: `pause-${index + 1}`, handler: () => undefined })),
  }).execute();
  return { result, sleeps: vi.mocked(ctx.step.sleep).mock.calls };
}

describe("retiring a Deploy's old copies", () => {
  it("ends done once an attempt leaves nothing running, pausing between attempts", async () => {
    const { result, sleeps } = await cleanup([[machine3], []]);
    expect(result).toEqual({ state: "done" });
    expect(sleeps).toEqual([["pause-1", "1m"]]);
    expect(writes).toEqual([
      { waiting: "run-1", left: [] },
      { waiting: "run-1", left: [machine3] },
      { end: "run-1", state: "done" },
    ]);
  });

  it("ends left running, not failed, naming each copy still running after the last attempt", async () => {
    const { result, sleeps } = await cleanup([[machine3], [machine3], [machine3], []]);
    expect(result).toEqual({ state: "left_running", left: [machine3] });
    expect(sleeps).toHaveLength(2);
    expect(writes.at(-1)).toEqual({ end: "run-1", state: "left_running", left: [machine3] });
    expect(answers).toEqual([[]]);
  });

  it("is taken over by a newer Deployment of the same Environment", () => {
    expect(createRetireOld(new Inngest({ id: "test" })).opts.cancelOn).toEqual([{
      event: expect.objectContaining({ name: "config/deployment.admitted" }),
      match: "data.environmentId",
      if: "async.data.deploymentId != event.data.deploymentId",
    }]);
  });

  it("marks the row cancelled when Inngest cancels a cleanup, under either function id, and skips any other", async () => {
    const cancel = (functionId: string) => new InngestTestEngine({
      function: createCancelRetireOld(new Inngest({ id: "ployz-cloud" })),
      events: [{ name: "inngest/function.cancelled", data: { function_id: functionId, run_id: "run-1" } }],
    }).execute();

    await cancel("retire-old");
    await cancel("ployz-cloud-retire-old");
    expect((await cancel("ployz-cloud-run-volume")).result).toEqual({ skipped: true });
    expect(writes).toEqual([{ end: "run-1", state: "cancelled" }, { end: "run-1", state: "cancelled" }]);
  });

  it("marks the row failed with the error once its retries run out", async () => {
    await invokeOnFailure(createRetireOld(new Inngest({ id: "test" })), "run-1", requested);
    expect(writes).toEqual([{ end: "run-1", state: "failed", message: "the run failed" }]);
  });
});
