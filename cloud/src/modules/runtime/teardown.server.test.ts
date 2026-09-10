import type {
  Client,
  DeployOutcome,
  ExecutionError,
  MachineId,
} from "@ployz/sdk";
import { it as effectIt } from "@effect/vitest";
import { Cause, Effect, Exit, Layer } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  InngestClient,
  InngestEventSendError,
} from "#/modules/inngest/client";
import {
  makeOrganizationRuntimeLayer,
} from "#/modules/runtime/organization-runtime.server";
import {
  makePloyzLayer,
} from "#/modules/runtime/ployz.server";
import {
  destroyEnvironmentActivity,
} from "#/modules/runtime/teardown-activities.server";
import { dispatchTeardownRequested } from "#/modules/runtime/teardown.server";

const volume = {
  kind: "docker_volume" as const,
  id: {
    machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
    name: "data",
  },
};

describe("teardown provider outcomes", () => {
  effectIt.effect("preserves project failure evidence and finalizes its session", () =>
    Effect.gen(function* () {
      let closed = 0;
      const calls: unknown[] = [];
      const projectOutcome: DeployOutcome<ExecutionError> = {
        type: "failed",
        completed: [],
        failed: {
          type: "operation",
          operation: { type: "remove_volume", id: volume.id },
          error: { type: "cancelled" },
        },
        unexecuted: [],
      };
      const connections = [{ tailcat: "tailcat://candidate" }];
      const client = asTestDouble<Client>()({
        destroyProject: async (
          ...args: Parameters<Client["destroyProject"]>
        ) => {
          calls.push(args);
          return projectOutcome;
        },
        removeVolumes: async () => {
          throw new Error("independent volume fallback must not run");
        },
        close: async () => {
          closed += 1;
        },
      });
      const ployz = makePloyzLayer({
        connect: async () => client,
      });
      const runtime = makeOrganizationRuntimeLayer(() =>
        Effect.succeed({ kind: "ready", generation: "grant-1", connections }),
      ).pipe(Layer.provide(ployz));

      const result = yield* Effect.scoped(
        destroyEnvironmentActivity({
          organizationId: "org-1",
          target: {
            environmentId: "env-1",
            projectId: "project-1",
            projectName: "app-production",
            cloudName: "acme/app/Production",
          },
          confirmDataLoss: [volume],
        }),
      ).pipe(Effect.provide(runtime));

      expect(result).toEqual(projectOutcome);
      expect(calls).toEqual([["app-production", { confirmed: [volume] }, true]]);
      expect(closed).toBe(1);
    }),
  );

  it("keeps its pending row retryable when dispatch fails", async () => {
    const failing = new Inngest({ id: "teardown-dispatch-fail-test" });
    failing.send = async () => {
      throw new Error("Inngest unavailable");
    };
    const exit = await Effect.runPromise(
      dispatchTeardownRequested("attempt-1").pipe(
        Effect.provideService(InngestClient, failing),
        Effect.exit,
      ),
    );

    expect(Exit.isFailure(exit)).toBe(true);
    if (Exit.isFailure(exit)) {
      expect(Cause.squash(exit.cause)).toBeInstanceOf(InngestEventSendError);
    }
  });
});
