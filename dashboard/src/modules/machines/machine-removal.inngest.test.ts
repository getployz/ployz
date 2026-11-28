import { InngestTestEngine, mockCtx } from "@inngest/test";
import type { MachineId } from "@ployz/sdk";
import { Inngest } from "inngest";
import { Effect } from "effect";
import { describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  createCancelMachineRemove,
  createProcessMachineRemove,
  MACHINE_REMOVE_RETRIES,
} from "#/modules/machines/machine-removal.inngest";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/modules/inngest/run.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

describe("machine remove Inngest boundary", () => {
  it("rejects malformed request events before an activity can run", async () => {
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [
        {
          name: "machine/remove.requested",
          data: { attemptId: "", extra: "forward-compatible" },
        },
      ],
    }).execute();

    expect(output.result).toEqual({
      attemptId: null,
      status: "invalid",
      skipped: true,
    });
    expect(output.ctx.step.run).not.toHaveBeenCalled();
  });

  it("ignores cancellation events owned by another function", async () => {
    const output = await new InngestTestEngine({
      function: createCancelMachineRemove(new Inngest({ id: "ployz-cloud" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: "ployz-cloud-process-environment-deployment",
            run_id: "run-1",
          },
        },
      ],
    }).execute();

    expect(output.result).toEqual({ skipped: true });
  });

  it("cancels the attempt when Inngest cancels the app's process-machine-remove", async () => {
    const output = await new InngestTestEngine({
      function: createCancelMachineRemove(new Inngest({ id: "ployz-cloud" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: "ployz-cloud-process-machine-remove", run_id: "run-1" },
        },
      ],
      steps: [{ id: "cancel-machine-remove", handler: () => ({ cancelled: true }) }],
    }).execute();

    expect(output.result).toEqual({ closed: { cancelled: true } });
  });

  it("rejects malformed cancellation envelopes in the owned step", async () => {
    const output = await new InngestTestEngine({
      function: createCancelMachineRemove(new Inngest({ id: "test" })),
      events: [
        {
          name: "inngest/function.cancelled",
          data: { function_id: "process-machine-remove" },
        },
      ],
    }).execute();

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Inngest event envelope is invalid.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
  });

  it("fails the attempt, and releases nothing, when the Server refuses its removal", async () => {
    const attemptId = "00000000-0000-4000-8000-000000000a01";
    const released = vi.fn();
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [{ name: "machine/remove.requested", data: { attemptId } }],
      steps: [
        { id: "claim-machine-remove", handler: () => ({ kind: "ready", attempt: { id: attemptId } }) },
        { id: "observe-leaving-server", handler: () => ({ machine: { id: "machine-1", name: "two" }, writes: ["data"] }) },
        { id: "remove-machine", handler: () => ({ kind: "refused", failureCode: "conflict", message: "Server two answers and holds copies of Volumes data; removing it without a reset leaves them behind. No changes made." }) },
        { id: "complete-refused", handler: () => undefined },
        { id: "release-volume-lanes", handler: released },
        { id: "release-server", handler: released },
      ],
    }).execute();

    expect(output.result).toEqual({ attemptId, status: "failed" });
    expect(released).not.toHaveBeenCalled();
  });

  it("leaves a superseded attempt alone: nothing is released, completed or synced", async () => {
    const attemptId = "00000000-0000-4000-8000-000000000a02";
    const touched = vi.fn();
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [{ name: "machine/remove.requested", data: { attemptId } }],
      steps: [
        { id: "claim-machine-remove", handler: () => ({ kind: "ready", attempt: { id: attemptId } }) },
        { id: "observe-leaving-server", handler: () => ({ machine: { id: "machine-1", name: "two" }, writes: ["data"] }) },
        { id: "remove-machine", handler: () => ({ kind: "superseded" }) },
        ...["release-volume-lanes", "release-server", "complete-succeeded", "complete-refused"].map((id) => ({ id, handler: touched })),
      ],
    }).execute();

    expect(output.result).toEqual({ attemptId, status: "superseded", skipped: true });
    expect(touched).not.toHaveBeenCalled();
    expect(output.ctx.step.sendEvent).not.toHaveBeenCalled();
  });

  it("releases the Server its removal forgot, retrying release without forgetting it again", async () => {
    const attemptId = "00000000-0000-4000-8000-000000000a03";
    const removed = vi.fn(() => ({ kind: "removed", last: false, generation: "1" }));
    const released = vi.fn(() => ({ kind: "others_remain" }));
    const output = await new InngestTestEngine({
      function: createProcessMachineRemove(new Inngest({ id: "test" })),
      events: [{ name: "machine/remove.requested", data: { attemptId } }],
      steps: [
        { id: "claim-machine-remove", handler: () => ({ kind: "ready", attempt: { id: attemptId, organizationId: "org-1", machineId: "machine-1" } }) },
        { id: "observe-leaving-server", handler: () => ({ machine: { id: "machine-1", name: "two" }, writes: ["data"] }) },
        { id: "remove-machine", handler: removed },
        { id: "release-volume-lanes", handler: () => [] },
        { id: "release-server", handler: released },
        { id: "complete-succeeded", handler: () => undefined },
        { id: "request-cluster-domain-sync", handler: () => undefined },
      ],
    }).execute();

    expect(output.result).toEqual({ attemptId, status: "succeeded" });
    expect(removed).toHaveBeenCalledTimes(1);
    expect(released).toHaveBeenCalled();
  });
});

describe("process-machine-remove observing the leaving Server", () => {
  const attemptId = "00000000-0000-4000-8000-000000000a04";
  const leaving = "leaving" as MachineId;
  const session = asTestDouble<PloyzSession>()({
    watchFirstFrame: () => Effect.succeed({
      machines: [{ machine: { id: leaving, name: "fsn-2" }, membership: "up" }],
      containers: [],
    }),
  });
  const runEffect = makeInngestEffectRunner(<A, E>(program: Effect.Effect<A, E, OrganizationRuntime | Database | SecretEncryption>) =>
    Effect.runPromise(program.pipe(
      Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntime["Service"]>()({
        open: () => Effect.succeed({ status: "connected" as const, connected: session }),
      })),
      Effect.provideService(Database, asTestDouble<Database["Service"]>()({})),
      Effect.provideService(SecretEncryption, asTestDouble<SecretEncryption["Service"]>()({})),
    ))) as typeof runInngestEffect;
  const unanswered = () => Effect.succeed({ copies: [], unanswered: [leaving] });

  const observeOnAttempt = (attempt: number) => new InngestTestEngine({
    function: createProcessMachineRemove(new Inngest({ id: "test" }), runEffect, unanswered),
    events: [{ name: "machine/remove.requested", data: { attemptId } }],
    steps: [{
      id: "claim-machine-remove",
      handler: () => ({ kind: "ready", attempt: { id: attemptId, organizationId: "org-1", machineId: leaving } }),
    }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), attempt }),
  }).executeStep("observe-leaving-server");

  it.each([0, MACHINE_REMOVE_RETRIES - 1])(
    "fails on attempt %i while the leaving Server reads up and its copies do not answer",
    async (attempt) => {
      const output = await observeOnAttempt(attempt);
      expect(output.error).toMatchObject({ name: "LeavingServerUnanswered" });
      expect(output.step.data).toBeUndefined();
    },
  );

  it("takes the unanswered Server as writing nothing on the step's last try", async () => {
    const output = await observeOnAttempt(MACHINE_REMOVE_RETRIES);
    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ machine: { id: leaving, name: "fsn-2" }, writes: [] });
  });
});
