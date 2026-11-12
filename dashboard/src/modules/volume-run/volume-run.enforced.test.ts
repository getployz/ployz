import type { ContainerId, MachineId } from "@ployz/sdk";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSession, RpcError } from "#/modules/runtime/ployz.server";
import { askCopy, refusedByEnforcement, SwitchAttemptError } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

const asker = { organizationId: "00000000-0000-4000-8000-000000000e01", dockerVolume: "shop-production_vol-vol1", volumeName: "db", gen: 3 };
const server = { id: "web-2".padEnd(32, "0") as MachineId, name: "web-2" };

/** A Server's PermissionDenied as the SDK hands it on, naming the capability it refused. */
const refusal = (capability: string) => new RpcError({
  code: "permission_denied",
  message: "This server refused it.",
  details: { capability, next: "ployz device enforce --off from an approved device", cause: [] },
});

/** The real askCopy, over a session whose SetVolumeCopy rejects with `rejection` as the SDK does. */
const asking = (rejection: InstanceType<typeof RpcError>) =>
  askCopy(asker, server, { kind: "mirror", newest: "12", slot: "1" }, {
    kind: "writer", at: "12", epoch: 2, start: { kind: "kept", container_id: "c".repeat(64) as ContainerId },
  }).pipe(
    // No approval rides on these asks, so nothing is recorded.
    Effect.provideService(Database, asTestDouble<typeof Database.Service>()({})),
    Effect.provideService(OrganizationRuntime, {
      cancel: () => Effect.void,
      open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
        volumeSwitch: () => Effect.fail(new PloyzProviderError({ operation: "volume switch", cause: rejection })),
      }) }),
    }),
  );

const ask = (rejection: InstanceType<typeof RpcError>) => Effect.runPromise(asking(rejection).pipe(Effect.result));

describe("a Server that enforces device approval", () => {
  it("answers a refused start as a refusal the run ends on, not a failure it retries", async () => {
    const answer = await ask(refusal("ployz.volume.copy.set.v1"));
    expect(answer).toMatchObject({
      _tag: "Success",
      success: {
        ok: false,
        reason: "enforced",
        copy: null,
        message: "web-2 enforces device approval, so moving db needs your passkey. Deploy the move again from the dashboard, or run ployz volume move from an approved device.",
      },
    });
  });

  it("leaves another Server refusal a failure the step retries", async () => {
    const failure = await Effect.runPromise(Effect.flip(asking(refusal("ployz.restart.device-approval.v2"))));
    expect(failure).toBeInstanceOf(SwitchAttemptError);
  });

  it("names a refused seed as the Server's enforcement", () => {
    const seed = new PloyzProviderError({ operation: "volume switch", cause: refusal("ployz.volume.seed.v1") });
    const restart = new PloyzProviderError({ operation: "volume switch", cause: refusal("ployz.restart.device-approval.v2") });
    expect([refusedByEnforcement(seed), refusedByEnforcement(restart)]).toEqual([true, false]);
  });
});
