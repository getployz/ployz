import type { Client, MachineId } from "@ployz/sdk";
import { it as effectIt } from "@effect/vitest";
import { Cause, Effect, Exit, Layer } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  InngestClient,
  InngestEventSendError,
} from "#/modules/inngest/client";
import type { DialTenant } from "#/modules/runtime/dial-entry";
import {
  makeOrganizationRuntimeLayer,
} from "#/modules/runtime/organization-runtime.server";
import {
  makePloyzLayer,
  SdkSurfaceNotShipped,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  composeEnvironmentDestroy,
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
  effectIt.effect("finalizes its runtime session inside the durable activity", () =>
    Effect.gen(function* () {
      let closed = 0;
      const tenant = {
        relayUrl: "wss://relay.example.test",
        bearer: "tenant-token",
        pairing: "ppair_test",
        preferredMachineId: "machine-a",
        enrolledMachineIds: ["machine-a"],
      } satisfies DialTenant;
      const ployz = makePloyzLayer({
        connect: async () =>
          asTestDouble<Client>()({
            destroyProject: async () => undefined,
            close: async () => {
              closed += 1;
            },
          }),
      });
      const runtime = makeOrganizationRuntimeLayer(() =>
        Effect.succeed({ kind: "ready", tenant }),
      ).pipe(Layer.provide(ployz));

      yield* Effect.scoped(
        destroyEnvironmentActivity({
          organizationId: "org-1",
          target: {
            environmentId: "env-1",
            projectId: "project-1",
            namespace: "app-production",
            cloudName: "acme/app/Production",
            identities: [],
          },
        }),
      ).pipe(Effect.provide(runtime));

      expect(closed).toBe(1);
    }),
  );

  it("stops after destroyProject succeeds", async () => {
    await expect(
      Effect.runPromise(
        composeEnvironmentDestroy(
          asTestDouble<PloyzSession>()({
            destroyProject: () => Effect.void,
            removeVolumes: () => Effect.die("must not run"),
          }),
          {
            namespace: "app-production",
            identities: [volume],
          },
        ),
      ),
    ).resolves.toBeNull();
  });

  it("falls back through truthful not-shipped outcomes", async () => {
    const result = await Effect.runPromise(
      composeEnvironmentDestroy(
        asTestDouble<PloyzSession>()({
          destroyProject: () =>
            Effect.fail(
              new SdkSurfaceNotShipped({
                surface: "destroyProject",
                ticket: "getployz/ployz2#253",
              }),
            ),
          removeVolumes: (request: Parameters<PloyzSession["removeVolumes"]>[0]) => {
            expect(request).toEqual({ volumes: [volume.id], force: false });
            return Effect.succeed([{ id: volume.id, outcome: { status: "removed" } }]);
          },
        }),
        {
          namespace: "app-production",
          identities: [volume],
        },
      ),
    );

    expect(result?.destroyed).toEqual([volume.id]);
  });

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
